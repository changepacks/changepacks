//! # changepacks-rust
//!
//! Rust project support for changepacks.
//!
//! Implements project discovery and version management for Cargo.toml files. Uses `toml_edit`
//! for non-destructive parsing to preserve file formatting, comments, and whitespace. Handles
//! both single crates and Cargo workspace configurations.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::RustProjectFinder;

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use changepacks_core::{Language, Package};
use changepacks_utils::{
    assign_preserving_decor, read_and_parse, replace_version_keep_prefix,
    requirement_needs_rewrite, write_finalized, write_toml_table_version,
};
use toml_edit::DocumentMut;

/// Default publish command for a single-crate `Cargo.toml`.
///
/// Kept as a `pub(crate) const` here so `RustPackage::default_publish_command`
/// and `RustWorkspace::default_publish_command` (which each have their own
/// workspace-scoped variant below) reference ONE source of truth. Every
/// other language crate — `changepacks-python`, `changepacks-dart`,
/// `changepacks-java`, `changepacks-csharp` — already exposes this same
/// `PUBLISH_COMMAND` const; this fills the Rust-crate gap so the pattern is
/// uniform and a future edit lives in one place.
pub(crate) const PUBLISH_COMMAND: &str = "cargo publish";

/// Default dry-run publish command for a single-crate `Cargo.toml`.
///
/// Paired with `PUBLISH_COMMAND` so both live next to each other for
/// package-scope callers.
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = "cargo publish --dry-run";

/// Read and parse a Cargo.toml file, preserving the raw content for format finalization.
///
/// Returns both the raw file content and the parsed `DocumentMut` to enable
/// [`changepacks_utils::write_finalized`] to preserve formatting, comments,
/// and the complete trailing-whitespace suffix.
///
/// The read-then-parse-with-context sequence lives in
/// [`changepacks_utils::read_and_parse`] — the mirror of
/// [`changepacks_utils::write_finalized`] — so only the `Cargo.toml` label and
/// the `toml_edit` parser stay here.
///
/// # Errors
/// Returns error if the file cannot be read or the TOML cannot be parsed.
pub(crate) async fn read_and_parse_cargo_toml(path: &Path) -> Result<(String, DocumentMut)> {
    read_and_parse(path, "Cargo.toml", str::parse::<DocumentMut>).await
}

/// Update the `[package].version` key of the `Cargo.toml` at `path` to
/// `new_version`, using `toml_edit` to preserve the file's formatting,
/// comments, and trailing-newline shape.
///
/// This helper only handles the simple `[package].version = "X.Y.Z"` case
/// used by [`crate::package::RustPackage`]. Workspace roots — where the
/// bump lives in `[workspace.package].version` and must also fan out into
/// `[workspace.dependencies]` path deps and virtual-workspace tables —
/// stay in [`crate::workspace::RustWorkspace::update_version`] because
/// they need much more than a single key rewrite.
///
/// The whole read → table-like guard → `[package]` creation →
/// decor-preserving assign → trailing-whitespace-preserving write pipeline
/// lives in [`changepacks_utils::write_toml_table_version`], because
/// `changepacks-python`'s `write_pyproject_version` was the same skeleton
/// modulo the manifest label, the table key, and one Python-only
/// `project.dynamic` guard, and `crates/AGENTS.md` forbids importing one
/// language crate into another. This wrapper stays so the `Cargo.toml`
/// key/label pair is bound in ONE place and every call site inside this crate
/// is unchanged; the Rust manifest has no extra rule, so the validator is a
/// no-op.
///
/// An empty `[package]` table is created if missing. The explicit
/// `Table::new()` in the shared helper matters: plain
/// `doc["package"]["version"] = ...` auto-creates an INLINE table
/// (`package = { version = ... }`) at the top of the document instead of a
/// proper `[package]` header.
///
/// The version assignment goes through
/// [`changepacks_utils::assign_preserving_decor`] so an end-of-line comment on
/// the version line survives the bump.
///
/// # Errors
/// Returns error if the file cannot be read, the TOML cannot be parsed,
/// `package` is present but not table-like, or the write fails.
pub(crate) async fn write_cargo_package_version(path: &Path, new_version: &str) -> Result<()> {
    write_toml_table_version(path, "Cargo.toml", "package", new_version, |_| Ok(())).await
}

/// Reject a `Cargo.toml` whose top-level `package` key exists but is NOT
/// table-like (e.g. `package = 3` or `package = "not-a-table"`), and report
/// whether the key is present at all.
///
/// Both writers that materialize `[package]` need the SAME two facts before
/// touching the document — "is the existing `package` item safe to index
/// into?" and "does it already exist?" — and both previously open-coded the
/// identical `is_some_and(|package| !package.is_table_like())` check plus a
/// byte-identical `bail!`. Extracted here beside [`is_workspace_marker`] and
/// [`workspace_dependencies_table_mut`] so the manifest-shape assumption AND
/// its user-visible message live in ONE place, matching the repo-wide "one
/// decoder, one place" convention.
///
/// [`crate::workspace::RustWorkspace::update_version`] is now the only caller
/// inside this crate — [`write_cargo_package_version`] reaches the same guard
/// through [`changepacks_utils::write_toml_table_version`] — and it reuses the
/// returned flag to drive its hybrid/virtual-root branch, so the extraction
/// adds no extra lookup relative to the previous hand-rolled pair.
///
/// Guarding BEFORE any mutation is the point: `toml_edit` indexing assignment
/// would otherwise silently replace the scalar and rewrite the manifest.
///
/// The body itself now lives in
/// [`changepacks_utils::ensure_toml_table_like`], because
/// `changepacks-python`'s `ensure_project_table_like` was the same function
/// modulo the key name and the manifest label, and `crates/AGENTS.md` forbids
/// importing one language crate into another. This wrapper stays so the
/// `Cargo.toml`-specific key/label pair is bound in ONE place and every call
/// site inside this crate is unchanged.
///
/// # Errors
/// Returns an error naming `path` when `package` is present but not table-like.
pub(crate) fn ensure_package_table_like(doc: &DocumentMut, path: &Path) -> Result<bool> {
    changepacks_utils::ensure_toml_table_like(doc, path, "package", "Cargo.toml")
}

/// Rewrite the `version` of a `[workspace.dependencies]` entry that also
/// carries a `path`, keeping the entry's range prefix and its `toml_edit`
/// decor, and report whether anything was written.
///
/// Both `[workspace.dependencies]` fan-outs in
/// [`crate::workspace::RustWorkspace`] — the workspace-version sync in
/// `update_version` and the member-version sync in
/// `update_workspace_dependencies` — previously open-coded the SAME five
/// steps: match `as_table_like_mut` (which accepts BOTH inline-table deps
/// `foo = { path = "...", version = "..." }` AND sub-table deps
/// `[workspace.dependencies.foo]`, while string deps `foo = "1.0"` yield
/// `None` and are skipped), require an existing `path` key, read the current
/// `version` as a string, decide whether this entry is in scope, then rewrite
/// it. Only the decision differs, so it is injected as `accept` and everything
/// else lives here — matching the repo-wide "one decoder, one place"
/// convention already followed by
/// [`changepacks_utils::assign_preserving_decor`] and
/// [`workspace_dependencies_table_mut`].
///
/// The `version` slot is looked up ONCE through `get_mut`, and the current
/// specifier is read back out of that same mutable borrow as a shared
/// reborrow; NLL ends the reborrow at
/// [`changepacks_utils::replace_version_keep_prefix`], which yields an OWNED
/// bumped string, so the later mutable use of the slot is legal.
/// `TableLike` exposes no `Index`/`[]` operator, so the value is rewritten in
/// place through `get_mut` — a `version` key that does not already exist is
/// NEVER inserted.
///
/// Returns `false` — writing nothing — when the item is not table-like, has no
/// `path`, has no string `version`, or `accept` rejects the current specifier.
pub(crate) fn sync_path_dependency_version(
    value: &mut toml_edit::Item,
    next_version: &str,
    accept: impl FnOnce(&str) -> bool,
) -> bool {
    let Some(dep) = value.as_table_like_mut() else {
        return false;
    };
    if dep.get("path").is_none() {
        return false;
    }
    let Some(slot) = dep.get_mut("version") else {
        return false;
    };
    let Some(current_version) = slot.as_str() else {
        return false;
    };
    if !accept(current_version) {
        return false;
    }
    let bumped = replace_version_keep_prefix(current_version, next_version);
    assign_preserving_decor(slot, &bumped);
    true
}

/// Index the bumped Rust packages by name, so a manifest walk can answer
/// "does this dependency entry name a package that just moved?" in O(1).
///
/// Packages of other languages, and Rust packages missing a name or a version,
/// contribute nothing a Cargo requirement could be retargeted to, so they are
/// dropped here rather than at each dependency entry. An empty map is
/// therefore also the "nothing to do" signal every caller uses to skip its
/// manifest read entirely.
pub(crate) fn bumped_rust_package_versions<'a>(
    packages: &[&'a dyn Package],
) -> HashMap<&'a str, &'a str> {
    packages
        .iter()
        .filter(|package| package.language() == Language::Rust)
        .filter_map(|package| Some((package.name()?, package.version()?)))
        .collect()
}

/// Rewrite every local path-dependency requirement in one dependency table
/// that names a bumped package, reporting whether anything was written.
///
/// The per-entry shape gates (table-like, an existing `path`, an existing
/// string `version`) and the decor-preserving in-place rewrite live in
/// [`sync_path_dependency_version`]; the in-scope decision is
/// [`changepacks_utils::requirement_needs_rewrite`]. Aliased entries
/// (`alias = { package = "real" }`) resolve through
/// [`crate::finder::effective_dependency_name`], so they bind to the package
/// they actually name.
fn sync_dependency_table_pins(
    dependencies: &mut dyn toml_edit::TableLike,
    package_versions: &HashMap<&str, &str>,
) -> bool {
    let mut any_updated = false;
    for (dependency_key, dependency) in dependencies.iter_mut() {
        let package_name =
            crate::finder::effective_dependency_name(dependency_key.get(), dependency);
        let Some(&next_version) = package_versions.get(package_name) else {
            continue;
        };
        any_updated |= sync_path_dependency_version(dependency, next_version, |current_version| {
            requirement_needs_rewrite(current_version, next_version)
        });
    }
    any_updated
}

/// Rewrite every local path-dependency requirement in `doc` — the workspace
/// root's `[workspace.dependencies]` table AND the ordinary
/// `[dependencies]` / `[dev-dependencies]` / `[build-dependencies]` tables,
/// including their `[target.'cfg(..)'.*]` forms — reporting whether anything
/// was written.
///
/// Both halves matter, and for different manifests. A workspace root pins its
/// members in `[workspace.dependencies]` while the members inherit with
/// `dep = { workspace = true }`; a workspace that does NOT use inheritance has
/// each member pin its siblings directly in its own `[dependencies]`. Cargo
/// refuses to resolve the workspace when EITHER shape goes stale, so one walk
/// covers both instead of a root-only rule that silently skips the second.
///
/// The table list is [`crate::finder::CARGO_DEPENDENCY_TABLES`], the same set
/// the finder scans for release-graph edges, so "which tables can name a local
/// package?" has one definition.
pub(crate) fn sync_dependency_pins(
    doc: &mut DocumentMut,
    package_versions: &HashMap<&str, &str>,
) -> bool {
    let mut any_updated = false;

    if let Some(workspace_dependencies) = workspace_dependencies_table_mut(doc) {
        any_updated |= sync_dependency_table_pins(workspace_dependencies, package_versions);
    }

    for (table_name, _) in crate::finder::CARGO_DEPENDENCY_TABLES {
        if let Some(dependencies) = doc
            .get_mut(table_name)
            .and_then(toml_edit::Item::as_table_like_mut)
        {
            any_updated |= sync_dependency_table_pins(dependencies, package_versions);
        }
    }

    if let Some(targets) = doc
        .get_mut("target")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        for (_, target) in targets.iter_mut() {
            let Some(target_table) = target.as_table_like_mut() else {
                continue;
            };
            for (table_name, _) in crate::finder::CARGO_DEPENDENCY_TABLES {
                if let Some(dependencies) = target_table
                    .get_mut(table_name)
                    .and_then(toml_edit::Item::as_table_like_mut)
                {
                    any_updated |= sync_dependency_table_pins(dependencies, package_versions);
                }
            }
        }
    }

    any_updated
}

/// Read the `Cargo.toml` at `path`, retarget every local path-dependency
/// requirement naming a bumped package, and write it back only when something
/// actually changed.
///
/// The no-change early return is what keeps an untouched manifest
/// byte-identical: `toml_edit` round-trips faithfully, but skipping the write
/// entirely also skips the mtime churn that would make a release commit list
/// manifests it did not modify.
///
/// # Errors
/// Returns an error if the manifest cannot be read, cannot be parsed as TOML,
/// or cannot be written back.
pub(crate) async fn sync_manifest_dependency_pins(
    path: &Path,
    package_versions: &HashMap<&str, &str>,
) -> Result<bool> {
    let (cargo_toml_raw, mut cargo_toml) = read_and_parse_cargo_toml(path).await?;
    if !sync_dependency_pins(&mut cargo_toml, package_versions) {
        return Ok(false);
    }
    write_finalized(path, cargo_toml.to_string(), &cargo_toml_raw, "Cargo.toml").await?;
    Ok(true)
}

/// Return `true` for a `toml_edit::Item` whose value is table-like with
/// `workspace = true` — the shape Cargo uses to mark either a
/// `[dependencies]` entry as inheriting from `[workspace.dependencies]`
/// (`dep = { workspace = true }`) or a `[package]` scalar as inheriting
/// from `[workspace.package]` (`version.workspace = true`, which `toml_edit`
/// parses as a dotted-key table `version = { workspace = true }`).
///
/// Shared by [`finder`](crate::finder) (`workspace_dep_names` and the
/// `inherits_workspace` chain in `visit`) and [`workspace`](crate::workspace)
/// (`RustWorkspace::update_version`'s hybrid-root inheritance guard) so the
/// "is this an inherited-version / inherited-dep marker" decision lives in ONE
/// place — matching the repo-wide "one decoder, one place" convention
/// (`is_regular_file`, `should_mark_changed`, `lookup_by_path_or_language`, …).
/// Byte-identical semantics to the previous hand-rolled chains:
/// `as_table_like()` returns `None` for scalars, `.get("workspace")` returns
/// `None` when the key is missing, `.as_bool()` returns `None` for non-bool
/// values, and each `None` path collapses to `false` via `.unwrap_or(false)`.
#[must_use]
pub(crate) fn is_workspace_marker(item: &toml_edit::Item) -> bool {
    item.as_table_like()
        .and_then(|t| t.get("workspace"))
        .and_then(toml_edit::Item::as_bool)
        .unwrap_or(false)
}

/// Look up the `[workspace.dependencies]` table for mutation, mirroring the
/// `doc.get_mut("workspace").and_then(|w| w.get_mut("dependencies")).and_then(|d| d.as_table_mut())`
/// chain that was previously open-coded twice in
/// [`workspace`](crate::workspace): once in `RustWorkspace::update_version`
/// (the workspace-version fan-out into path deps) and once in
/// `RustWorkspace::update_workspace_dependencies` (the member-version sync).
/// Extracted so the manifest-shape assumption lives in ONE place, matching the
/// precedent set by `workspace_package_str` in [`finder`](crate::finder).
///
/// Returns `None` on any missing hop — no `[workspace]`, no
/// `[workspace.dependencies]`, or a non-table `dependencies` item — leaving
/// each caller free to keep its own control flow (`if let Some(..)` vs a
/// `let ... else { return Ok(()) }`). Returning the borrowed `toml_edit`
/// handle rather than an owned copy is what keeps formatting, indentation, and
/// key order untouched.
pub(crate) fn workspace_dependencies_table_mut(
    doc: &mut DocumentMut,
) -> Option<&mut toml_edit::Table> {
    doc.get_mut("workspace")
        .and_then(|w| w.get_mut("dependencies"))
        .and_then(|d| d.as_table_mut())
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_cargo_package_version_preserves_complete_trailing_whitespace() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // A gnarly suffix (space, tab, CRLF, space, LF) that a plain
        // `DocumentMut::to_string()` would collapse — only `finalize_content`
        // restores it byte for byte.
        let suffix = " \t\r\n \n";
        fs::write(
            &cargo_toml,
            format!("[package]\nname = \"x\"\nversion = \"1.0.0\"{suffix}"),
        )
        .unwrap();

        write_cargo_package_version(&cargo_toml, "2.0.0")
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&cargo_toml).unwrap(),
            format!("[package]\nname = \"x\"\nversion = \"2.0.0\"{suffix}")
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, "[package]\nversion = \"1.0.0\"\n").unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        test_support::set_readonly(&cargo_toml, true);

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result = write_cargo_package_version(&cargo_toml, "2.0.0").await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        test_support::set_readonly(&cargo_toml, false);

        let err = result.expect_err("write to a readonly Cargo.toml must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&cargo_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_non_table_package_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, "package = 3\n").unwrap();

        let err = write_cargo_package_version(&cargo_toml, "2.0.0")
            .await
            .expect_err("non-table package item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&cargo_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("non-table [package]"),
            "error chain should mention the non-table package item, got: {chain}"
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_non_table_package_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // A scalar top-level `package` key. The sibling test above pins the
        // ERROR TEXT; this one pins the guard's actual reason for existing —
        // it must reject BEFORE the `cargo_toml["package"]["version"] = ...`
        // assignment ever runs, so the manifest on disk is never clobbered.
        let original = "package = \"not-a-table\"\n\n[dependencies]\nserde = \"1\"\n";
        fs::write(&cargo_toml, original).unwrap();

        let err = write_cargo_package_version(&cargo_toml, "1.0.1")
            .await
            .expect_err("non-table package item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("has a non-table [package] item"),
            "error chain should name the non-table package guard, got: {chain}"
        );

        // Byte-for-byte, not line-for-line: a partial or reformatted write is
        // exactly the manifest destruction the guard prevents.
        assert_eq!(
            fs::read(&cargo_toml).unwrap(),
            original.as_bytes(),
            "a rejected bump must leave the manifest byte-identical"
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_creates_proper_package_header() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // A manifest with NO [package] table at all. Without the explicit
        // `Table::new()` guard, `doc["package"]["version"] = ...` auto-creates
        // an INLINE table (`package = { version = "0.0.1" }`) at the top of
        // the document instead of a proper `[package]` header.
        fs::write(&cargo_toml, "[workspace]\nmembers = [\"a\"]\n").unwrap();

        write_cargo_package_version(&cargo_toml, "0.0.1")
            .await
            .unwrap();

        let written = fs::read_to_string(&cargo_toml).unwrap();
        assert!(
            written.lines().any(|line| line.trim() == "[package]"),
            "output must contain a literal [package] header line, got: {written}"
        );
        assert!(
            written.contains("version = \"0.0.1\""),
            "output must contain the new version, got: {written}"
        );
        assert!(
            !written.contains("package = {"),
            "output must not use the inline-table form, got: {written}"
        );
        assert!(
            written.lines().any(|line| line.trim() == "[workspace]")
                && written.contains("members = [\"a\"]"),
            "output must preserve the existing [workspace] section, got: {written}"
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_preserves_version_line_decor() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // The version line carries an end-of-line comment, which `toml_edit`
        // stores as the VALUE's suffix decor. Assigning a freshly built value
        // would drop it, silently deleting the user's comment on a routine
        // bump. Whole-file round trip: only the version literal may change.
        let original = "# release-managed manifest\n[package]\nname = \"x\"\nversion = \"1.0.0\" # pinned by release tooling\nedition = \"2024\"\n";
        fs::write(&cargo_toml, original).unwrap();

        write_cargo_package_version(&cargo_toml, "2.0.0")
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&cargo_toml).unwrap(),
            original.replace("1.0.0", "2.0.0"),
            "only the version literal may change"
        );
    }

    /// Renders a realistically formatted `Cargo.toml` at `version`.
    ///
    /// Every construct here is one a re-serializing TOML writer silently
    /// normalizes away: a header comment, `[dependencies]` declared BEFORE
    /// `[package]` (non-canonical table order that Cargo accepts but no
    /// serializer would reproduce), an end-of-line comment on the version
    /// line, a multi-line array with a trailing comma, an inline table with
    /// custom interior spacing, and the blank lines between tables.
    fn round_trip_manifest(version: &str) -> String {
        format!(
            concat!(
                "# demo crate manifest - this header comment must survive a bump\n",
                "\n",
                "[dependencies]\n",
                "serde = {{ version = \"1\",  features = [\"derive\"] }}\n",
                "demo-core = {{ path = \"../core\", version = \"0.1\" }}\n",
                "\n",
                "[package]\n",
                "name = \"demo\"\n",
                "version = \"{version}\" # bumped by changepacks\n",
                "edition = \"2024\"\n",
                "categories = [\n",
                "    \"development-tools\",\n",
                "    \"command-line-utilities\",\n",
                "]\n",
            ),
            version = version
        )
    }

    /// Format preservation is a hard project constraint, but the existing
    /// whole-file assertion above uses a minimal three-key fixture. This
    /// asserts COMPLETE-FILE equality over a realistic manifest (not a
    /// `contains` check), mirroring the `changepacks-python` and
    /// `changepacks-dart` round-trip tests, so any reformatting `toml_edit`
    /// or [`changepacks_utils::write_finalized`] performs - dropped comment, reordered table,
    /// collapsed array or inline table, lost blank line - fails the test
    /// rather than silently rewriting a user's manifest.
    #[tokio::test]
    async fn test_write_cargo_package_version_preserves_comments_and_table_order() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, round_trip_manifest("1.2.3")).unwrap();

        write_cargo_package_version(&cargo_toml, "2.0.0")
            .await
            .expect("a well-formed manifest must be writable");

        assert_eq!(
            fs::read_to_string(&cargo_toml).unwrap(),
            round_trip_manifest("2.0.0"),
            "only the version literal may change; everything else must be byte-identical"
        );
    }

    /// Every rejection path of [`sync_path_dependency_version`] must write
    /// NOTHING — the entry, and the whole manifest around it, stays
    /// byte-identical. In order: a registry dependency (a scalar, so not
    /// table-like), a table without `path` (also a registry dependency, just
    /// spelled long-hand), a `path` dependency that intentionally pins no
    /// `version`, a `version` that is not a string, and an entry `accept`
    /// refuses. The last case is the one that keeps out-of-scope path deps
    /// (e.g. a member the current bump does not touch) from being rewritten.
    #[rstest]
    #[case::not_table_like("\"1.0\"", true)]
    #[case::no_path_key("{ version = \"1.0\" }", true)]
    #[case::no_version_key("{ path = \"../dep\" }", true)]
    #[case::non_string_version("{ path = \"../dep\", version = 1 }", true)]
    #[case::rejected_by_accept("{ path = \"../dep\", version = \"1.0\" }", false)]
    fn test_sync_path_dependency_version_rejections_write_nothing(
        #[case] spec: &str,
        #[case] accept: bool,
    ) {
        let original = format!("dep = {spec}\n");
        let mut doc: DocumentMut = original.parse().unwrap();

        let written = sync_path_dependency_version(
            doc.get_mut("dep").expect("fixture declares `dep`"),
            "2.0.0",
            |_| accept,
        );

        assert!(!written, "rejected entry reported a write: {spec}");
        assert_eq!(
            doc.to_string(),
            original,
            "a rejected entry must leave the manifest byte-identical"
        );
    }

    // The accepted path rewrites the `version` slot in place: the range prefix
    // survives, and so does the entry's end-of-line comment, which a
    // freshly-built value would drop.
    #[test]
    fn test_sync_path_dependency_version_rewrites_accepted_entry_in_place() {
        let mut doc: DocumentMut = "dep = { path = \"../dep\", version = \"^1.2.3\" } # pinned\n"
            .parse()
            .unwrap();

        let written = sync_path_dependency_version(
            doc.get_mut("dep").expect("fixture declares `dep`"),
            "2.0.0",
            |current| current == "^1.2.3",
        );

        assert!(written);
        assert_eq!(
            doc.to_string(),
            "dep = { path = \"../dep\", version = \"^2.0.0\" } # pinned\n"
        );
    }

    fn bumped(pairs: &[(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
        pairs.iter().copied().collect()
    }

    /// A hand-maintained manifest carrying every shape the pin walk must reach
    /// AND every construct a re-serializing writer would normalize away:
    /// a comment, an alias entry, aligned `=` columns, a sub-table entry, a
    /// target-specific table, a registry dependency, a path-only dependency,
    /// and a still-covering partial requirement.
    fn pin_manifest(core: &str, tool: &str) -> String {
        format!(
            concat!(
                "[workspace]\n",
                "members = [\"crates/*\"]\n",
                "\n",
                "# hand-maintained pins\n",
                "[workspace.dependencies]\n",
                "rayon        = \"1.12\"\n",
                "demo-core    = {{ path = \"crates/demo-core\", version = \"{core}\", default-features = false }}\n",
                "aliased-core = {{ package = \"demo-core\", path = \"crates/demo-core\", version = \"{core}\" }}\n",
                "demo-app     = {{ path = \"crates/demo-app\", version = \"0.2\" }}\n",
                "\n",
                "[dependencies]\n",
                "demo-tool = {{ path = \"crates/demo-tool\", version = \"{tool}\" }} # direct sibling pin\n",
                "path-only = {{ path = \"crates/path-only\" }}\n",
                "\n",
                "[dev-dependencies.demo-tool]\n",
                "path = \"crates/demo-tool\"\n",
                "version = \"{tool}\"\n",
                "\n",
                "[target.'cfg(unix)'.build-dependencies]\n",
                "demo-tool = {{ path = \"crates/demo-tool\", version = \"{tool}\" }}\n",
            ),
            core = core,
            tool = tool
        )
    }

    /// The whole point of the walk: every table that can name a local package
    /// is retargeted — `[workspace.dependencies]`, the ordinary tables, a
    /// sub-table entry, and a `[target.'cfg(..)']` table — while a registry
    /// entry, a path-only entry, and a still-covering `"0.2"` requirement are
    /// left alone and the file is otherwise byte-identical.
    #[test]
    fn test_sync_dependency_pins_rewrites_every_local_table_and_preserves_format() {
        let mut doc: DocumentMut = pin_manifest("=0.2.1", "^0.2.1").parse().unwrap();

        let written = sync_dependency_pins(
            &mut doc,
            &bumped(&[("demo-core", "0.3.0"), ("demo-tool", "0.3.0")]),
        );

        assert!(written);
        assert_eq!(doc.to_string(), pin_manifest("=0.3.0", "^0.3.0"));
    }

    /// `demo-app` moves 0.2.1 -> 0.2.2, which its `"0.2"` requirement still
    /// admits, so nothing is written at all — not even a no-op rewrite that
    /// would narrow the author's chosen range to an exact version.
    #[test]
    fn test_sync_dependency_pins_leaves_a_still_covering_requirement_untouched() {
        let original = pin_manifest("=0.2.1", "^0.2.1");
        let mut doc: DocumentMut = original.parse().unwrap();

        let written = sync_dependency_pins(&mut doc, &bumped(&[("demo-app", "0.2.2")]));

        assert!(
            !written,
            "a still-covering requirement must not be rewritten"
        );
        assert_eq!(doc.to_string(), original);
    }

    #[tokio::test]
    async fn test_sync_manifest_dependency_pins_skips_the_write_when_nothing_matches() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        let original = pin_manifest("=0.2.1", "^0.2.1");
        fs::write(&cargo_toml, &original).unwrap();

        // A readonly manifest proves the no-match path never reaches the write:
        // the guard is what keeps this `Ok(false)` instead of a write error.
        test_support::set_readonly(&cargo_toml, true);
        let result =
            sync_manifest_dependency_pins(&cargo_toml, &bumped(&[("other", "9.9.9")])).await;
        test_support::set_readonly(&cargo_toml, false);

        assert!(!result.expect("a no-match manifest must not be written"));
        assert_eq!(fs::read_to_string(&cargo_toml).unwrap(), original);
    }

    #[tokio::test]
    async fn test_sync_manifest_dependency_pins_writes_the_retargeted_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, pin_manifest("=0.2.1", "^0.2.1")).unwrap();

        let written =
            sync_manifest_dependency_pins(&cargo_toml, &bumped(&[("demo-core", "0.3.0")]))
                .await
                .unwrap();

        assert!(written);
        assert_eq!(
            fs::read_to_string(&cargo_toml).unwrap(),
            pin_manifest("=0.3.0", "^0.2.1")
        );
    }
}
