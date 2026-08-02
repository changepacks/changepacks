use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::HashSet,
    fmt::{Debug, Display},
    path::Path,
};

use anyhow::Result;
use colored::{ColoredString, Colorize};

use crate::{config::Config, package::Package, update_type::UpdateType, workspace::Workspace};

/// Map one path byte to its separator-normalized form: backslash becomes
/// forward slash, every other byte is returned unchanged.
///
/// Single source of truth for the normalization applied by
/// [`cmp_normalized_paths`], which must map BOTH sides through this helper --
/// normalizing only one side would make the comparison non-total.
const fn normalized_separator_byte(byte: u8) -> u8 {
    if byte == b'\\' { b'/' } else { byte }
}

/// Compare paths after normalizing backslashes to forward slashes.
///
/// Both sides are mapped through [`normalized_separator_byte`]; they must stay
/// in lockstep, so the shared helper is the only place the rule is written.
///
/// The comparison reads the OS-encoded bytes directly via
/// [`std::ffi::OsStr::as_encoded_bytes`] instead of going through a lossy
/// `&str` view. That encoding is stable and cross-platform, and it is an
/// ASCII-compatible self-synchronizing superset of UTF-8: a backslash byte can
/// never occur inside a multi-byte sequence, and byte-lexicographic order
/// matches Unicode scalar-value order. Every valid-Unicode path therefore
/// compares byte-identically to the previous `to_string_lossy` view.
///
/// The difference is the paths that are NOT valid Unicode, which `read_dir`
/// can hand us on any filesystem that does not validate encoding:
/// `to_string_lossy` rewrites every invalid unit to the same `U+FFFD`, so two
/// distinct such paths could compare `Equal` here and make this comparison a
/// non-total pre-order over paths. Reading the encoded bytes keeps them
/// distinct, which is what the raw-path tie-breaker in `cmp_paths` previously
/// had to rescue.
#[must_use]
pub fn cmp_normalized_paths(left: &Path, right: &Path) -> Ordering {
    let left_bytes = left.as_os_str().as_encoded_bytes();
    let right_bytes = right.as_os_str().as_encoded_bytes();

    left_bytes
        .iter()
        .copied()
        .map(normalized_separator_byte)
        .cmp(right_bytes.iter().copied().map(normalized_separator_byte))
}

/// Discriminated union of Package (single project) or Workspace (monorepo root).
///
/// Provides unified interface for operations on both package and workspace projects,
/// delegating to the appropriate trait implementation. Workspaces sort before packages
/// in ordering comparisons.
#[derive(Debug)]
pub enum Project {
    /// Monorepo workspace root containing multiple packages
    Workspace(Box<dyn Workspace>),
    /// Single versioned package
    Package(Box<dyn Package>),
}

/// Format an optional version string as `v{version}`, or `"unknown"` when absent.
///
/// Single source of truth for the "unknown"/`v{v}` version-display policy shared
/// by [`Project::format_line`] and `changepacks_utils::display_update`'s
/// current-version rendering. A future rewording (e.g. "no version" instead of
/// "unknown", or a different prefix) now lands in exactly one place across both
/// crates. Byte-identical to the previously open-coded `map_or_else` copies.
///
/// Returns [`Cow`] so the `None` branch borrows the `"unknown"` literal instead
/// of allocating a `String` for a constant; only the `Some` branch, which must
/// build `v{version}`, allocates.
#[must_use]
pub fn format_version_display(version: Option<&str>) -> Cow<'static, str> {
    version.map_or(Cow::Borrowed("unknown"), |v| Cow::Owned(format!("v{v}")))
}

impl Project {
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Workspace(workspace) => workspace.name(),
            Self::Package(package) => package.name(),
        }
    }

    /// Return the project's name, or the shared `"noname"` sentinel when
    /// the manifest supplies no `name` field.
    ///
    /// Centralizes the `project.name().unwrap_or("noname")` sentinel. The
    /// only callers are `commands/tree.rs::tree_roots`, which orders tree
    /// roots by name, and `Project::format_line`, through which the rest of
    /// the tree output reaches the sentinel indirectly. A future rename of
    /// the sentinel (e.g. `"anonymous"`, `"unknown"`) lands in one place
    /// instead of every open-coded call site. Byte-identical output to the
    /// previous open-coded pattern.
    #[must_use]
    pub fn name_or_noname(&self) -> &str {
        self.name().unwrap_or("noname")
    }

    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Workspace(workspace) => workspace.version(),
            Self::Package(package) => package.version(),
        }
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Workspace(workspace) => workspace.path(),
            Self::Package(package) => package.path(),
        }
    }

    #[must_use]
    pub fn relative_path(&self) -> &Path {
        match self {
            Self::Workspace(workspace) => workspace.relative_path(),
            Self::Package(package) => package.relative_path(),
        }
    }

    /// # Errors
    /// Returns error if the underlying `update_version` call fails.
    pub async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        match self {
            Self::Workspace(workspace) => workspace.update_version(update_type).await?,
            Self::Package(package) => package.update_version(update_type).await?,
        }
        Ok(())
    }

    /// # Errors
    /// Returns error if the underlying `check_changed` call fails.
    pub fn check_changed(&mut self, path: &Path) -> Result<()> {
        match self {
            Self::Workspace(workspace) => workspace.check_changed(path)?,
            Self::Package(package) => package.check_changed(path)?,
        }
        Ok(())
    }

    #[must_use]
    pub fn is_changed(&self) -> bool {
        match self {
            Self::Workspace(workspace) => workspace.is_changed(),
            Self::Package(package) => package.is_changed(),
        }
    }

    #[must_use]
    pub fn dependencies(&self) -> &HashSet<String> {
        match self {
            Self::Workspace(workspace) => workspace.dependencies(),
            Self::Package(package) => package.dependencies(),
        }
    }

    pub fn add_dependency(&mut self, dependency: &str) {
        match self {
            Self::Workspace(workspace) => workspace.add_dependency(dependency),
            Self::Package(package) => package.add_dependency(dependency),
        }
    }

    pub fn set_name(&mut self, name: String) {
        match self {
            Self::Workspace(workspace) => workspace.set_name(name),
            Self::Package(package) => package.set_name(name),
        }
    }

    #[must_use]
    pub fn language(&self) -> crate::Language {
        match self {
            Self::Workspace(workspace) => workspace.language(),
            Self::Package(package) => package.language(),
        }
    }

    #[must_use]
    pub fn is_publishable_by_default(&self) -> bool {
        match self {
            Self::Workspace(workspace) => workspace.is_publishable_by_default(),
            Self::Package(package) => package.is_publishable_by_default(),
        }
    }

    #[must_use]
    pub fn is_dry_run_publishable_by_default(&self) -> bool {
        match self {
            Self::Workspace(workspace) => workspace.is_dry_run_publishable_by_default(),
            Self::Package(package) => package.is_dry_run_publishable_by_default(),
        }
    }

    /// # Errors
    /// Returns error if the underlying publish call fails to spawn.
    pub async fn publish(&self, config: &Config) -> Result<crate::publish::PublishOutput> {
        match self {
            Self::Workspace(workspace) => workspace.publish(config).await,
            Self::Package(package) => package.publish(config).await,
        }
    }

    /// Run the publish command in dry-run mode.
    ///
    /// Returns `Ok(None)` when dry-run is not supported for this project's
    /// language and no override is configured in `config.publish_dry_run`.
    ///
    /// # Errors
    /// Returns error if the underlying dry-run publish call fails to spawn.
    pub async fn dry_run_publish(
        &self,
        config: &Config,
    ) -> Result<Option<crate::publish::PublishOutput>> {
        match self {
            Self::Workspace(workspace) => workspace.dry_run_publish(config).await,
            Self::Package(package) => package.dry_run_publish(config).await,
        }
    }

    /// Render the project's canonical one-line label (`[Workspace - Node] name
    /// (v1.0.0) - path`) with optional version override.
    ///
    /// `version_override` is used verbatim when supplied (typically a
    /// pre-formatted "v1.0.0 -> v1.1.0 (minor)" upgrade string from
    /// `changepacks_utils::display_update`); when `None`, the current version
    /// is rendered via [`format_version_display`]. Extracted from `Display` so
    /// `commands/tree.rs::format_project_line` reuses the exact same base
    /// formatting.
    #[must_use]
    pub fn format_line(&self, version_override: Option<&str>) -> String {
        // Both variants render identical bytes except for the header prefix:
        // Workspace → "[Workspace - {lang}]", Package → "[{lang}]". Destructure
        // once so a future label/formatting change only needs to be applied in
        // one place.
        let (label_prefix, lang, rel_path) = match self {
            Self::Workspace(w) => ("Workspace - ", w.language(), w.relative_path()),
            Self::Package(p) => ("", p.language(), p.relative_path()),
        };
        // Route the None-override branch through `format_version_display` so
        // the "unknown"/"v{v}" formatting policy lives in exactly one place —
        // shared with `commands/tree.rs::format_project_line`'s CLI display path.
        // Borrowed via Cow to skip a per-line String copy when an override is
        // supplied, and to skip the allocation entirely for the "unknown" case.
        let version: Cow<'_, str> =
            version_override.map_or_else(|| format_version_display(self.version()), Cow::Borrowed);
        format!(
            "{} {} {} {} {}",
            format!("[{label_prefix}{lang}]").bright_blue().bold(),
            self.name_or_noname().bright_white().bold(),
            format!("({version})").bright_green(),
            "-".bright_cyan(),
            // `Path::display().to_string()` already produces an owned String.
            // Going through `Colorize for &str` would deref-coerce and copy it
            // a second time into `ColoredString::input`; `From<String> for
            // ColoredString` moves the existing allocation instead. The
            // resulting `ColoredString` is field-for-field identical, so the
            // rendered bytes are unchanged.
            ColoredString::from(rel_path.display().to_string()).bright_black(),
        )
    }
}

/// Variant-asserting accessors for tests.
///
/// Every language crate's `finder.rs` test module repeated the same six-line
/// `match project { Project::Package(pkg) => { ... } _ => panic!("Expected
/// Package") }` shape purely to reach the inner trait object, and the wording
/// of the catch-all arm had already drifted between copies ("Expected
/// Package" vs "expected package"). These accessors own the assertion once, so
/// a call site shrinks to a single `let` binding and every failure reports the
/// same message.
///
/// Gated on `cfg(any(test, feature = "test-support"))` for the same reason
/// [`crate::test_support`] is: this is test scaffolding, so it must not appear
/// in the crate's shipped API surface. Downstream crates opt in through the
/// `test-support` feature in their `[dev-dependencies]`.
///
/// `#[track_caller]` makes a failed assertion point at the test line that
/// called the accessor rather than at this file.
#[cfg(any(test, feature = "test-support"))]
impl Project {
    /// Borrow the inner [`Package`] trait object.
    ///
    /// # Panics
    /// Panics when this project is a [`Project::Workspace`].
    #[must_use]
    #[track_caller]
    pub fn expect_package(&self) -> &dyn Package {
        match self {
            Self::Package(package) => package.as_ref(),
            Self::Workspace(_) => panic!("expected Project::Package, got Project::Workspace"),
        }
    }

    /// Mutably borrow the inner [`Package`] trait object.
    ///
    /// Exists because two finder tests (`csharp` and `dart`'s
    /// `test_projects_mut`) reach through `projects_mut()` to flip
    /// `set_changed`. There is deliberately no `expect_workspace_mut` twin: no
    /// in-tree test needs one, and an unused accessor is dead weight.
    ///
    /// # Panics
    /// Panics when this project is a [`Project::Workspace`].
    #[must_use]
    #[track_caller]
    pub fn expect_package_mut(&mut self) -> &mut dyn Package {
        match self {
            Self::Package(package) => package.as_mut(),
            Self::Workspace(_) => panic!("expected Project::Package, got Project::Workspace"),
        }
    }

    /// Borrow the inner [`Workspace`] trait object.
    ///
    /// # Panics
    /// Panics when this project is a [`Project::Package`].
    #[must_use]
    #[track_caller]
    pub fn expect_workspace(&self) -> &dyn Workspace {
        match self {
            Self::Workspace(workspace) => workspace.as_ref(),
            Self::Package(_) => panic!("expected Project::Workspace, got Project::Package"),
        }
    }
}

impl PartialEq for Project {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Project {}

impl PartialOrd for Project {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Shared language-then-name comparator for the Workspace × Workspace and
/// Package × Package arms of `Project::cmp`. Both arms follow the same
/// precedence — language first, then `(Some, Some) < (Some, None) < (None,
/// Some) < (None, None)` — and the divergence is the `(None, None)` tie-breaker:
/// Workspace falls back to version comparison, while Package has no additional
/// primary tie-breaker.
fn cmp_lang_then_name(
    lhs: (crate::Language, Option<&str>),
    rhs: (crate::Language, Option<&str>),
    none_none: impl FnOnce() -> Ordering,
) -> Ordering {
    let lang_ord = lhs.0.cmp(&rhs.0);
    if lang_ord != Ordering::Equal {
        return lang_ord;
    }
    match (lhs.1, rhs.1) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => none_none(),
    }
}

fn cmp_paths(lhs_relative: &Path, lhs_raw: &Path, rhs_relative: &Path, rhs_raw: &Path) -> Ordering {
    cmp_normalized_paths(lhs_relative, rhs_relative).then_with(|| lhs_raw.cmp(rhs_raw))
}

impl Ord for Project {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Workspace(_), Self::Package(_)) => Ordering::Less,
            (Self::Package(_), Self::Workspace(_)) => Ordering::Greater,
            (Self::Workspace(w1), Self::Workspace(w2)) => cmp_lang_then_name(
                (w1.language(), w1.name()),
                (w2.language(), w2.name()),
                || w1.version().unwrap_or("").cmp(w2.version().unwrap_or("")),
            )
            .then_with(|| cmp_paths(w1.relative_path(), w1.path(), w2.relative_path(), w2.path())),
            (Self::Package(p1), Self::Package(p2)) => cmp_lang_then_name(
                (p1.language(), p1.name()),
                (p2.language(), p2.name()),
                || Ordering::Equal,
            )
            .then_with(|| cmp_paths(p1.relative_path(), p1.path(), p2.relative_path(), p2.path())),
        }
    }
}

impl Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_line(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Language;
    use crate::test_support::{MockPackage, MockWorkspace};
    use rstest::rstest;

    fn ws(name: Option<&str>, version: Option<&str>, language: Language) -> Project {
        Project::Workspace(Box::new(MockWorkspace::new(name, version, language)))
    }

    fn pkg(name: Option<&str>, version: Option<&str>, language: Language) -> Project {
        Project::Package(Box::new(MockPackage::new(name, version, language)))
    }

    #[test]
    fn test_project_workspace_name() {
        let workspace = MockWorkspace::new(Some("test-ws"), Some("1.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        assert_eq!(project.name(), Some("test-ws"));
    }

    #[test]
    fn test_project_package_name() {
        let package = MockPackage::new(Some("test-pkg"), Some("1.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        assert_eq!(project.name(), Some("test-pkg"));
    }

    #[test]
    fn test_project_workspace_version() {
        let workspace = MockWorkspace::new(Some("test"), Some("2.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        assert_eq!(project.version(), Some("2.0.0"));
    }

    #[test]
    fn test_project_package_version() {
        let package = MockPackage::new(Some("test"), Some("3.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        assert_eq!(project.version(), Some("3.0.0"));
    }

    #[test]
    fn test_project_workspace_path() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        assert_eq!(project.path(), Path::new("/test/package.json"));
    }

    #[test]
    fn test_project_package_path() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        assert_eq!(project.path(), Path::new("/test/Cargo.toml"));
    }

    #[test]
    fn test_project_workspace_relative_path() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        assert_eq!(project.relative_path(), Path::new("package.json"));
    }

    #[test]
    fn test_project_package_relative_path() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        assert_eq!(project.relative_path(), Path::new("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_project_workspace_update_version() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let mut project = Project::Workspace(Box::new(workspace));
        let result = project.update_version(UpdateType::Minor).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_project_package_update_version() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let mut project = Project::Package(Box::new(package));
        let result = project.update_version(UpdateType::Patch).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_project_workspace_check_changed() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let mut project = Project::Workspace(Box::new(workspace));
        let result = project.check_changed(Path::new("/test/src/index.js"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_project_package_check_changed() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let mut project = Project::Package(Box::new(package));
        let result = project.check_changed(Path::new("/test/src/main.rs"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_project_workspace_is_changed() {
        let mut workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        workspace.is_changed = true;
        let project = Project::Workspace(Box::new(workspace));
        assert!(project.is_changed());
    }

    #[test]
    fn test_project_package_is_changed() {
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.is_changed = true;
        let project = Project::Package(Box::new(package));
        assert!(project.is_changed());
    }

    #[test]
    fn test_project_workspace_dependencies() {
        let mut workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        workspace.dependencies.insert("dep1".to_string());
        let project = Project::Workspace(Box::new(workspace));
        assert!(project.dependencies().contains("dep1"));
    }

    #[test]
    fn test_project_package_dependencies() {
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.dependencies.insert("dep2".to_string());
        let project = Project::Package(Box::new(package));
        assert!(project.dependencies().contains("dep2"));
    }

    #[test]
    fn test_project_workspace_add_dependency() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let mut project = Project::Workspace(Box::new(workspace));
        project.add_dependency("new-dep");
        assert!(project.dependencies().contains("new-dep"));
    }

    #[test]
    fn test_project_package_add_dependency() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let mut project = Project::Package(Box::new(package));
        project.add_dependency("new-dep");
        assert!(project.dependencies().contains("new-dep"));
    }

    #[test]
    fn test_project_workspace_language() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Python);
        let project = Project::Workspace(Box::new(workspace));
        assert!(matches!(project.language(), Language::Python));
    }

    #[test]
    fn test_project_package_language() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Dart);
        let project = Project::Package(Box::new(package));
        assert!(matches!(project.language(), Language::Dart));
    }

    #[test]
    fn test_project_variants_are_publishable_by_default() {
        let workspace = ws(Some("workspace"), Some("1.0.0"), Language::Node);
        let package = pkg(Some("package"), Some("1.0.0"), Language::Rust);

        assert!(workspace.is_publishable_by_default());
        assert!(package.is_publishable_by_default());
        assert!(workspace.is_dry_run_publishable_by_default());
        assert!(package.is_dry_run_publishable_by_default());
    }

    #[tokio::test]
    async fn test_project_workspace_publish() {
        let temp_dir = std::env::temp_dir();
        let mut workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        workspace.path = temp_dir.join("package.json");
        let project = Project::Workspace(Box::new(workspace));
        let config = Config::default();
        let output = project.publish(&config).await.unwrap();
        assert!(output.success);
    }

    #[tokio::test]
    async fn test_project_package_publish() {
        let temp_dir = std::env::temp_dir();
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.path = temp_dir.join("Cargo.toml");
        let project = Project::Package(Box::new(package));
        let config = Config::default();
        let output = project.publish(&config).await.unwrap();
        assert!(output.success);
    }

    #[tokio::test]
    async fn test_project_workspace_dry_run_publish() {
        let temp_dir = std::env::temp_dir();
        let mut workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        workspace.path = temp_dir.join("package.json");
        let project = Project::Workspace(Box::new(workspace));
        let config = Config::default();

        // MockWorkspace.default_dry_run_publish_command() returns
        // Some("echo publish --dry-run").
        let output = project.dry_run_publish(&config).await.unwrap();
        assert!(output.is_some());
        let output = output.unwrap();
        assert!(output.success);
        assert!(output.stdout.contains("publish"));
    }

    #[tokio::test]
    async fn test_project_package_dry_run_publish() {
        let temp_dir = std::env::temp_dir();
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.path = temp_dir.join("Cargo.toml");
        let project = Project::Package(Box::new(package));
        let config = Config::default();

        let output = project.dry_run_publish(&config).await.unwrap();
        assert!(output.is_some());
        assert!(output.unwrap().success);
    }

    #[tokio::test]
    async fn test_project_package_dry_run_publish_propagates_language_override() {
        let temp_dir = std::env::temp_dir();
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::CSharp);
        package.path = temp_dir.join("Sample.csproj");
        let project = Project::Package(Box::new(package));
        let mut publish_dry_run = std::collections::BTreeMap::new();
        publish_dry_run.insert("csharp".to_string(), "echo dry-csharp".to_string());
        let config = Config {
            publish_dry_run,
            ..Config::default()
        };

        // A per-language `publish_dry_run` config entry (here: `csharp`)
        // resolves to a runnable command and takes precedence over the
        // crate's built-in `default_dry_run_publish_command()`, ensuring
        // user-configured dry-run commands win regardless of whether the
        // language provides its own default.
        let output = project.dry_run_publish(&config).await.unwrap();
        assert!(output.is_some());
        assert!(output.unwrap().success);
    }

    #[test]
    fn test_project_eq_same_workspace() {
        let w1 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert_eq!(p1, p2);
    }

    #[test]
    fn test_project_partial_ord() {
        let w1 = MockWorkspace::new(Some("a"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("b"), Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert!(p1.partial_cmp(&p2).is_some());
    }

    #[rstest]
    // Workspaces sort before packages regardless of language/name.
    #[case(
        ws(Some("test"), Some("1.0.0"), Language::Node),
        pkg(Some("test"), Some("1.0.0"), Language::Rust),
        Ordering::Less
    )]
    #[case(
        pkg(Some("test"), Some("1.0.0"), Language::Rust),
        ws(Some("test"), Some("1.0.0"), Language::Node),
        Ordering::Greater
    )]
    // Same variant + same language: order by name, with `Some(_) < None`.
    #[case(
        ws(Some("aaa"), Some("1.0.0"), Language::Node),
        ws(Some("bbb"), Some("1.0.0"), Language::Node),
        Ordering::Less
    )]
    #[case(
        ws(Some("test"), Some("1.0.0"), Language::Node),
        ws(None, Some("1.0.0"), Language::Node),
        Ordering::Less
    )]
    #[case(
        ws(None, Some("1.0.0"), Language::Node),
        ws(Some("test"), Some("1.0.0"), Language::Node),
        Ordering::Greater
    )]
    // Two nameless workspaces fall back to version comparison.
    #[case(
        ws(None, Some("1.0.0"), Language::Node),
        ws(None, Some("2.0.0"), Language::Node),
        Ordering::Less
    )]
    // A nameless workspace with no version substitutes the empty string, which
    // sorts before every real version.
    #[case(
        ws(None, None, Language::Node),
        ws(None, Some("1.0.0"), Language::Node),
        Ordering::Less
    )]
    #[case(
        ws(None, Some("1.0.0"), Language::Node),
        ws(None, None, Language::Node),
        Ordering::Greater
    )]
    #[case(
        pkg(Some("aaa"), Some("1.0.0"), Language::Rust),
        pkg(Some("bbb"), Some("1.0.0"), Language::Rust),
        Ordering::Less
    )]
    #[case(
        pkg(Some("test"), Some("1.0.0"), Language::Rust),
        pkg(None, Some("1.0.0"), Language::Rust),
        Ordering::Less
    )]
    #[case(
        pkg(None, Some("1.0.0"), Language::Rust),
        pkg(Some("test"), Some("1.0.0"), Language::Rust),
        Ordering::Greater
    )]
    // Two nameless packages are equal (packages have no version tie-break).
    #[case(
        pkg(None, Some("1.0.0"), Language::Rust),
        pkg(None, Some("1.0.0"), Language::Rust),
        Ordering::Equal
    )]
    fn test_project_ord(#[case] a: Project, #[case] b: Project, #[case] expected: Ordering) {
        assert_eq!(a.cmp(&b), expected);
    }

    #[test]
    fn test_project_ord_workspaces_by_language() {
        // Kept separate: asserts only that a language difference breaks the tie
        // (not a specific ordering), so it stays independent of the `Language`
        // enum's declared order.
        let w1 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Python);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert_ne!(p1.cmp(&p2), Ordering::Equal);
    }

    #[test]
    fn test_project_ord_packages_by_language() {
        let pkg1 = MockPackage::new(Some("test"), Some("1.0.0"), Language::Node);
        let pkg2 = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let p1 = Project::Package(Box::new(pkg1));
        let p2 = Project::Package(Box::new(pkg2));
        assert_ne!(p1.cmp(&p2), Ordering::Equal);
    }

    fn assert_path_order(mut projects: Vec<Project>, expected_paths: &[&str]) {
        let left_to_right = projects[0].cmp(&projects[1]);
        let right_to_left = projects[1].cmp(&projects[0]);

        assert_ne!(left_to_right, Ordering::Equal);
        assert_eq!(left_to_right, right_to_left.reverse());

        projects.sort();
        let sorted_paths: Vec<_> = projects
            .iter()
            .map(|project| project.path().to_string_lossy())
            .collect();
        assert_eq!(sorted_paths, expected_paths);
    }

    /// Direct coverage for the public `cmp_normalized_paths` contract.
    ///
    /// Every other assertion in this module reaches the function through the
    /// private `cmp_paths` wrapper, which layers a raw-path tie-break on top
    /// and therefore cannot observe the wrapper-free result — notably the
    /// `Equal` verdict for two paths that differ only in separator style.
    /// `cmp_normalized_paths` is public and re-exported from `lib.rs`, and
    /// `changepacks_utils::project_names::compare_paths` calls it directly, so
    /// the documented behaviour is pinned here on its own terms.
    #[rstest]
    // Separator style alone is not a difference: a mid-path backslash
    // normalizes to `/`, so the two spellings compare Equal.
    #[case(
        "packages\\core\\package.json",
        "packages/core/package.json",
        Ordering::Equal
    )]
    // Once normalized, ordering is decided by the bytes after the separator,
    // not by the separator byte itself (`\\` = 0x5C sorts after `/` = 0x2F,
    // so an un-normalized comparison would answer Greater here).
    #[case(
        "packages\\alpha\\package.json",
        "packages/beta/package.json",
        Ordering::Less
    )]
    #[case(
        "packages/beta/package.json",
        "packages\\alpha\\package.json",
        Ordering::Greater
    )]
    // Empty paths are accepted and compare Equal.
    #[case("", "", Ordering::Equal)]
    // A prefix sorts before the longer path that extends it.
    #[case("packages/core", "packages/core/package.json", Ordering::Less)]
    // Multi-byte components keep character order under byte-wise
    // normalization: `é` (U+00E9 -> C3 A9) precedes `한` (U+D55C -> ED 95 9C).
    #[case(
        "packages/éclair/package.json",
        "packages/한글/package.json",
        Ordering::Less
    )]
    #[case(
        "packages/한글/package.json",
        "packages/éclair/package.json",
        Ordering::Greater
    )]
    fn cmp_normalized_paths_compares_paths_ignoring_separator_style(
        #[case] left: &str,
        #[case] right: &str,
        #[case] expected: Ordering,
    ) {
        assert_eq!(
            cmp_normalized_paths(Path::new(left), Path::new(right)),
            expected,
        );
    }

    /// Build two paths that share pure-ASCII halves and differ ONLY in one
    /// INVALID encoding unit, so no lossless `&str` view can represent either.
    /// Same construction as `changepacks_utils::is_changepack_log`'s non-Unicode
    /// tests: raw bytes via `OsStringExt::from_vec` on Unix.
    #[cfg(unix)]
    fn distinct_non_unicode_paths() -> (std::ffi::OsString, std::ffi::OsString) {
        use std::os::unix::ffi::OsStringExt;

        let build = |invalid: u8| {
            let mut bytes = b"packages/alpha".to_vec();
            bytes.push(invalid);
            bytes.extend_from_slice(b"/package.json");
            std::ffi::OsString::from_vec(bytes)
        };
        (build(0xFF), build(0xFE))
    }

    /// Windows counterpart of the Unix `distinct_non_unicode_paths` above: two
    /// different unpaired high surrogates via `OsStringExt::from_wide` (WTF-8).
    #[cfg(windows)]
    fn distinct_non_unicode_paths() -> (std::ffi::OsString, std::ffi::OsString) {
        use std::os::windows::ffi::OsStringExt;

        let build = |invalid: u16| {
            let mut units: Vec<u16> = "packages/alpha".encode_utf16().collect();
            units.push(invalid);
            units.extend("/package.json".encode_utf16());
            std::ffi::OsString::from_wide(&units)
        };
        (build(0xD800), build(0xD801))
    }

    // Pins what the switch from `to_string_lossy` to `OsStr::as_encoded_bytes`
    // buys: the lossy view rewrites EVERY invalid encoding unit to the same
    // `U+FFFD`, so two genuinely different paths used to compare Equal. Callers
    // inside this workspace survived that false tie only because they append a
    // raw-path tie-breaker (`cmp_paths` here, `compare_paths` in
    // `changepacks-utils`); this exported comparator is now a total order on
    // its own. `read_dir` can hand such names to the finders on any filesystem
    // that does not validate Unicode.
    #[cfg(any(unix, windows))]
    #[test]
    fn cmp_normalized_paths_distinguishes_non_unicode_paths() {
        let (left, right) = distinct_non_unicode_paths();

        assert!(
            left.to_str().is_none() && right.to_str().is_none(),
            "test precondition: {left:?} and {right:?} must not be valid Unicode"
        );
        assert_eq!(
            left.to_string_lossy(),
            right.to_string_lossy(),
            "test precondition: the lossy views must collide on U+FFFD"
        );
        assert_ne!(
            cmp_normalized_paths(Path::new(&left), Path::new(&right)),
            Ordering::Equal,
            "distinct non-Unicode paths must not compare Equal"
        );
    }

    #[test]
    fn cmp_paths_normalizes_slashes_and_backslashes_before_ordering() {
        let backslash_relative = Path::new("packages\\alpha\\package.json");
        let slash_relative = Path::new("packages/beta/package.json");

        assert_eq!(
            cmp_paths(
                backslash_relative,
                Path::new("/repo/z/package.json"),
                slash_relative,
                Path::new("/repo/a/package.json"),
            ),
            Ordering::Less,
        );
        assert_eq!(
            cmp_paths(
                slash_relative,
                Path::new("/repo/a/package.json"),
                backslash_relative,
                Path::new("/repo/z/package.json"),
            ),
            Ordering::Greater,
        );
    }

    #[test]
    fn cmp_paths_orders_non_ascii_normalized_paths_lexicographically() {
        let latin_relative = Path::new("packages/éclair/package.json");
        let hangul_relative = Path::new("packages/한글/package.json");

        assert_eq!(
            cmp_paths(
                latin_relative,
                Path::new("/repo/z/package.json"),
                hangul_relative,
                Path::new("/repo/a/package.json"),
            ),
            Ordering::Less,
        );
        assert_eq!(
            cmp_paths(
                hangul_relative,
                Path::new("/repo/a/package.json"),
                latin_relative,
                Path::new("/repo/z/package.json"),
            ),
            Ordering::Greater,
        );
    }

    #[test]
    fn cmp_paths_uses_raw_path_to_order_normalized_key_collisions() {
        let backslash_relative = Path::new("packages\\core\\package.json");
        let slash_relative = Path::new("packages/core/package.json");
        let earlier_raw = Path::new("/repo/a/package.json");
        let later_raw = Path::new("/repo/z/package.json");

        assert_eq!(
            cmp_paths(backslash_relative, earlier_raw, slash_relative, later_raw,),
            Ordering::Less,
        );
        assert_eq!(
            cmp_paths(slash_relative, later_raw, backslash_relative, earlier_raw,),
            Ordering::Greater,
        );
        assert_eq!(
            cmp_paths(backslash_relative, earlier_raw, slash_relative, earlier_raw,),
            Ordering::Equal,
        );
    }

    #[test]
    fn test_project_ord_packages_with_equal_names_by_relative_path() {
        let later = Project::Package(Box::new(MockPackage::with_all(
            Some("same"),
            Some("1.0.0"),
            "/repo/z/Cargo.toml",
            "packages/z/Cargo.toml",
            Language::Rust,
        )));
        let earlier = Project::Package(Box::new(MockPackage::with_all(
            Some("same"),
            Some("1.0.0"),
            "/repo/a/Cargo.toml",
            "packages/a/Cargo.toml",
            Language::Rust,
        )));

        assert_path_order(
            vec![later, earlier],
            &["/repo/a/Cargo.toml", "/repo/z/Cargo.toml"],
        );
    }

    #[test]
    fn test_project_ord_workspaces_with_equal_names_by_relative_path() {
        let later = Project::Workspace(Box::new(MockWorkspace::with_all(
            Some("same"),
            Some("1.0.0"),
            "/repo/z/package.json",
            "workspaces/z/package.json",
            Language::Node,
        )));
        let earlier = Project::Workspace(Box::new(MockWorkspace::with_all(
            Some("same"),
            Some("1.0.0"),
            "/repo/a/package.json",
            "workspaces/a/package.json",
            Language::Node,
        )));

        assert_path_order(
            vec![later, earlier],
            &["/repo/a/package.json", "/repo/z/package.json"],
        );
    }

    #[test]
    fn test_project_ord_unnamed_packages_by_raw_path_after_normalization() {
        let earlier = Project::Package(Box::new(MockPackage::with_all(
            None,
            Some("1.0.0"),
            "/repo/a/Cargo.toml",
            "packages\\core\\Cargo.toml",
            Language::Rust,
        )));
        let later = Project::Package(Box::new(MockPackage::with_all(
            None,
            Some("1.0.0"),
            "/repo/z/Cargo.toml",
            "packages/core/Cargo.toml",
            Language::Rust,
        )));

        assert_path_order(
            vec![later, earlier],
            &["/repo/a/Cargo.toml", "/repo/z/Cargo.toml"],
        );
    }

    #[test]
    fn test_project_ord_unnamed_workspaces_by_raw_path_after_normalization() {
        let earlier = Project::Workspace(Box::new(MockWorkspace::with_all(
            None,
            Some("1.0.0"),
            "/repo/a/package.json",
            "workspaces\\root\\package.json",
            Language::Node,
        )));
        let later = Project::Workspace(Box::new(MockWorkspace::with_all(
            None,
            Some("1.0.0"),
            "/repo/z/package.json",
            "workspaces/root/package.json",
            Language::Node,
        )));

        assert_path_order(
            vec![later, earlier],
            &["/repo/a/package.json", "/repo/z/package.json"],
        );
    }

    #[rstest]
    #[case(ws(Some("my-workspace"), Some("1.0.0"), Language::Node), &["Workspace", "my-workspace", "v1.0.0"])]
    #[case(ws(None, Some("1.0.0"), Language::Node), &["noname"])]
    #[case(ws(Some("test"), None, Language::Node), &["unknown"])]
    #[case(pkg(Some("my-package"), Some("2.0.0"), Language::Rust), &["my-package", "v2.0.0"])]
    #[case(pkg(None, Some("1.0.0"), Language::Rust), &["noname"])]
    #[case(pkg(Some("test"), None, Language::Rust), &["unknown"])]
    fn test_project_display(#[case] project: Project, #[case] expected: &[&str]) {
        let display = format!("{project}");
        for &needle in expected {
            assert!(display.contains(needle), "{display:?} missing {needle:?}");
        }
    }

    /// Direct coverage for the public `format_version_display` contract.
    ///
    /// Every other assertion in this module reaches the function through
    /// `Display for Project`, which routes via `Project::format_line` and
    /// immediately interpolates the result into a `String`, so the
    /// borrowed-vs-owned `Cow` variant the doc comment promises is never
    /// observed.
    /// `format_version_display` is public, re-exported from `lib.rs`, and
    /// consumed cross-crate by `changepacks_utils::display_update`, so its
    /// allocation contract is pinned here on its own terms.
    #[test]
    fn format_version_display_borrows_unknown_and_allocates_only_for_some() {
        let absent = format_version_display(None);
        // The `None` branch hands back the `"unknown"` literal itself; an
        // `Cow::Owned` here would mean a `String` was allocated for a constant.
        assert!(matches!(&absent, Cow::Borrowed(_)), "{absent:?}");
        assert_eq!(absent, "unknown");

        let present = format_version_display(Some("1.2.3"));
        assert!(matches!(&present, Cow::Owned(_)), "{present:?}");
        assert_eq!(present, "v1.2.3");

        // An empty version is still `Some`, so it gets the `v` prefix rather
        // than falling back to the `None`-only `"unknown"` sentinel.
        let empty = format_version_display(Some(""));
        assert!(matches!(&empty, Cow::Owned(_)), "{empty:?}");
        assert_eq!(empty, "v");
    }

    /// Pins the `Some(version_override)` arm of `Project::format_line`.
    ///
    /// Every other test in this module reaches `format_line` through
    /// `Display`, which always passes `None`, so the override arm was only
    /// exercised indirectly from `changepacks-cli`. Assertions use substrings
    /// that survive ANSI colouring (`colored` wraps the whole `({version})`
    /// segment), matching the existing display cases above.
    #[test]
    fn test_project_format_line_uses_version_override_verbatim() {
        let project = pkg(Some("my-package"), Some("1.0.0"), Language::Rust);
        let override_text = "v1.0.0 -> v1.1.0 (minor)";

        let line = project.format_line(Some(override_text));

        assert!(
            line.contains(override_text),
            "{line:?} missing override {override_text:?}"
        );
        // The `None` arm would render the package's own version as `(v1.0.0)`;
        // its absence proves the override replaced it rather than being
        // appended alongside it.
        assert!(
            !line.contains("(v1.0.0)"),
            "{line:?} unexpectedly rendered the non-override version"
        );
        assert!(
            line.contains("my-package"),
            "{line:?} missing the project name"
        );
    }

    #[test]
    fn test_project_sort_stability() {
        let make_projects = || {
            vec![
                Project::Package(Box::new(MockPackage::new(
                    Some("charlie"),
                    Some("1.0.0"),
                    Language::Rust,
                ))),
                Project::Workspace(Box::new(MockWorkspace::new(
                    Some("alpha"),
                    Some("2.0.0"),
                    Language::Node,
                ))),
                Project::Package(Box::new(MockPackage::new(
                    Some("bravo"),
                    Some("0.1.0"),
                    Language::Node,
                ))),
                Project::Workspace(Box::new(MockWorkspace::new(
                    Some("delta"),
                    Some("3.0.0"),
                    Language::Python,
                ))),
                Project::Package(Box::new(MockPackage::new(
                    Some("echo"),
                    Some("1.0.0"),
                    Language::Dart,
                ))),
            ]
        };

        let mut first = make_projects();
        first.sort();
        let first_order: Vec<Option<&str>> = first.iter().map(|p| p.name()).collect();

        let mut second = make_projects();
        second.sort();
        let second_order: Vec<Option<&str>> = second.iter().map(|p| p.name()).collect();

        let mut third = make_projects();
        third.sort();
        let third_order: Vec<Option<&str>> = third.iter().map(|p| p.name()).collect();

        assert_eq!(first_order, second_order);
        assert_eq!(second_order, third_order);
    }

    #[test]
    fn test_project_sort_mixed() {
        let mut projects = [
            Project::Package(Box::new(MockPackage::new(
                Some("pkg-a"),
                Some("1.0.0"),
                Language::Node,
            ))),
            Project::Workspace(Box::new(MockWorkspace::new(
                Some("ws-b"),
                Some("1.0.0"),
                Language::Node,
            ))),
            Project::Package(Box::new(MockPackage::new(
                Some("pkg-c"),
                Some("1.0.0"),
                Language::Rust,
            ))),
            Project::Workspace(Box::new(MockWorkspace::new(
                Some("ws-d"),
                Some("1.0.0"),
                Language::Rust,
            ))),
        ];
        projects.sort();

        // All workspaces must come before all packages
        let workspace_count = projects
            .iter()
            .take_while(|p| matches!(p, Project::Workspace(_)))
            .count();
        assert_eq!(workspace_count, 2);

        let package_count = projects
            .iter()
            .skip(workspace_count)
            .filter(|p| matches!(p, Project::Package(_)))
            .count();
        assert_eq!(package_count, 2);
    }

    #[test]
    fn test_project_set_name_workspace() {
        // MockWorkspace's `Workspace` impl now consumes
        // `crate::impl_basic_accessors!()`, so `set_name` updates the
        // wrapped `name` field. `Project::set_name` delegates straight to
        // that impl, so the underlying rename must round-trip.
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let mut project = Project::Workspace(Box::new(workspace));
        project.set_name("new-name".to_string());
        assert_eq!(project.name(), Some("new-name"));
    }

    #[test]
    fn test_project_set_name_package() {
        // Same rationale as `test_project_set_name_workspace` — MockPackage
        // uses the shared basic-accessors macro, so `Project::set_name`
        // propagates through to the mock's `name` field.
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let mut project = Project::Package(Box::new(package));
        project.set_name("new-name".to_string());
        assert_eq!(project.name(), Some("new-name"));
    }

    /// Direct coverage for the `expect_package` / `expect_package_mut` /
    /// `expect_workspace` accessors shared by every language crate's
    /// `finder.rs` test module.
    ///
    /// The migrated call sites only ever exercise the matching-variant path,
    /// so the panicking arm — the whole reason the accessors exist — would
    /// otherwise go unasserted.
    #[test]
    fn expect_package_and_workspace_return_the_matching_variant() {
        let package = pkg(Some("my-package"), Some("2.0.0"), Language::Rust);
        assert_eq!(package.expect_package().name(), Some("my-package"));
        assert_eq!(package.expect_package().version(), Some("2.0.0"));

        let workspace = ws(Some("my-workspace"), Some("1.0.0"), Language::Node);
        assert_eq!(workspace.expect_workspace().name(), Some("my-workspace"));
        assert_eq!(workspace.expect_workspace().version(), Some("1.0.0"));
    }

    #[test]
    fn expect_package_mut_yields_a_borrow_that_mutates_the_project() {
        let mut project = pkg(Some("my-package"), Some("2.0.0"), Language::Rust);

        assert!(!project.expect_package_mut().is_changed());
        project.expect_package_mut().set_changed(true);
        assert!(project.expect_package().is_changed());
    }

    #[test]
    #[should_panic(expected = "expected Project::Package, got Project::Workspace")]
    fn expect_package_panics_on_a_workspace() {
        let workspace = ws(Some("my-workspace"), Some("1.0.0"), Language::Node);
        let _package = workspace.expect_package();
    }

    #[test]
    #[should_panic(expected = "expected Project::Package, got Project::Workspace")]
    fn expect_package_mut_panics_on_a_workspace() {
        let mut workspace = ws(Some("my-workspace"), Some("1.0.0"), Language::Node);
        let _package = workspace.expect_package_mut();
    }

    #[test]
    #[should_panic(expected = "expected Project::Workspace, got Project::Package")]
    fn expect_workspace_panics_on_a_package() {
        let package = pkg(Some("my-package"), Some("2.0.0"), Language::Rust);
        let _workspace = package.expect_workspace();
    }

    #[test]
    fn test_project_cmp_is_consistent_with_eq() {
        // Two workspaces with identical fields
        let w1 = MockWorkspace::new(Some("same"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("same"), Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert_eq!(p1, p2);
        assert_eq!(p1.cmp(&p2), Ordering::Equal);

        // Two packages with identical fields
        let pkg1 = MockPackage::new(Some("same"), Some("1.0.0"), Language::Rust);
        let pkg2 = MockPackage::new(Some("same"), Some("1.0.0"), Language::Rust);
        let pp1 = Project::Package(Box::new(pkg1));
        let pp2 = Project::Package(Box::new(pkg2));
        assert_eq!(pp1, pp2);
        assert_eq!(pp1.cmp(&pp2), Ordering::Equal);
    }
}
