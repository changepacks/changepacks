//! # changepacks-node
//!
//! Node.js project support for changepacks.
//!
//! Implements project discovery, version management, and workspace detection for package.json
//! files. Automatically detects the package manager (npm, pnpm, yarn, bun) by looking for
//! lock files and provides appropriate publish commands for each.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::NodeProjectFinder;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use changepacks_core::{Config, Language, is_regular_file};
use changepacks_utils::{detect_indent_str, finalize_content};
use serde::Serialize;
use tokio::fs::{read_to_string, write};

/// Read and parse a `package.json` file, returning both the raw content and
/// the parsed JSON value.
///
/// Used by both `write_package_json_version` and `NodeProjectFinder::visit`
/// to consolidate the "read file, parse JSON, attach context" sequence into
/// a single source of truth.
///
/// # Errors
/// Returns error if the file cannot be read or is not valid JSON.
pub(crate) async fn read_and_parse_package_json(
    path: &Path,
) -> Result<(String, serde_json::Value)> {
    let package_json_raw = read_to_string(path)
        .await
        .with_context(|| format!("Failed to read package.json {}", path.display()))?;
    let package_json: serde_json::Value = serde_json::from_str(&package_json_raw)
        .with_context(|| format!("Failed to parse package.json {}", path.display()))?;
    Ok((package_json_raw, package_json))
}

/// Update `package.json` at `path` to set its `version` field to `new_version`,
/// preserving the file's original indent size (via `detect_indent`) and its
/// trailing-newline shape (via `trailing_newline`).
///
/// Shared by `NodePackage::update_version` and `NodeWorkspace::update_version`
/// so both paths emit byte-identical output.
///
/// # Errors
/// Returns error if the file cannot be read, is not valid JSON, or the write
/// fails.
pub(crate) async fn write_package_json_version(path: &Path, new_version: &str) -> Result<()> {
    let (package_json_raw, mut package_json) = read_and_parse_package_json(path).await?;
    let indent_str = detect_indent_str(&package_json_raw);
    let Some(obj) = package_json.as_object_mut() else {
        anyhow::bail!(
            "package.json {} does not have a top-level JSON object",
            path.display()
        );
    };
    let new_value = serde_json::Value::String(new_version.to_string());
    if let Some(v) = obj.get_mut("version") {
        *v = new_value;
    } else {
        obj.insert("version".to_string(), new_value);
    }
    let ind = indent_str.as_bytes();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(ind);
    let writer = Vec::with_capacity(package_json_raw.len());
    let mut ser = serde_json::Serializer::with_formatter(writer, formatter);
    package_json.serialize(&mut ser)?;
    write(
        path,
        finalize_content(String::from_utf8(ser.into_inner())?, &package_json_raw),
    )
    .await
    .with_context(|| format!("Failed to write package.json {}", path.display()))?;
    Ok(())
}

/// Expand to the identical `default_publish_command` /
/// `default_dry_run_publish_command` pair used by both `NodePackage` and
/// `NodeWorkspace`.
///
/// Node cannot use `changepacks_core::impl_const_publish_commands!()`
/// because its publish command is determined at runtime by walking the
/// ancestor chain via `detect_package_manager_recursive`, not from a
/// compile-time const.
///
/// PATH wiring for lifecycle hooks — prepending `node_modules/.bin` so
/// `husky` and friends resolve during `bun publish` / `bun pm pack`
/// (working around oven-sh/bun#16071, #18055, #23594) — is deliberately
/// NOT part of this macro. `NodePackage` / `NodeWorkspace` override
/// `publish` / `dry_run_publish` wholesale and inject those dirs through
/// the async `run_publish_for_path` / `run_dry_run_publish_for_path` path
/// (`node_modules_bin_dirs_async`), so the `core` trait-default publish
/// flow (which passes no extra PATH dirs) is never reached for Node.
///
/// Invoked from inside an `impl Package for NodePackage` or `impl
/// Workspace for NodeWorkspace` block. Byte-identical expansion — the
/// previously hand-rolled bodies:
///
/// ```ignore
/// fn default_publish_command(&self) -> String {
///     detect_package_manager_recursive(&self.path, self.relative_path.components().count())
///         .publish_command()
///         .to_string()
/// }
/// fn default_dry_run_publish_command(&self) -> Option<String> {
///     Some(
///         detect_package_manager_recursive(&self.path, self.relative_path.components().count())
///             .dry_run_publish_command()
///             .to_string(),
///     )
/// }
/// ```
///
/// are replaced 1:1 by a single `crate::impl_node_publish_wiring!();`
/// invocation. Fully-qualified `::std::string::String` and
/// `::std::option::Option` make the macro hygienic — callers do not need
/// those types in scope at the invocation site.
///
/// Consumer requirement: the struct must have BOTH a `path: PathBuf` and a
/// `relative_path: PathBuf` field, with those exact spellings. The
/// `relative_path` (the manifest's path relative to the git root) bounds the
/// `detect_package_manager_recursive` ancestor walk to the repository root via
/// `relative_path.components().count()`, so a stray lockfile ABOVE the git root
/// cannot flip the publish command. Both `NodePackage` and `NodeWorkspace`
/// satisfy this — the only two intended callers.
macro_rules! impl_node_publish_wiring {
    () => {
        fn default_publish_command(&self) -> ::std::string::String {
            $crate::detect_package_manager_recursive(
                &self.path,
                self.relative_path.components().count(),
            )
            .publish_command()
            .to_string()
        }
        fn default_dry_run_publish_command(&self) -> ::std::option::Option<::std::string::String> {
            ::std::option::Option::Some(
                $crate::detect_package_manager_recursive(
                    &self.path,
                    self.relative_path.components().count(),
                )
                .dry_run_publish_command()
                .to_string(),
            )
        }
    };
}

pub(crate) use impl_node_publish_wiring;

/// Represents the detected Node.js package manager
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Bun,
}

/// Lockfile filenames and package-manager priority table.
/// Ordered: Bun (bun.lockb, bun.lock), Pnpm (pnpm-lock.yaml), Yarn (yarn.lock).
/// Used by both sync and async detectors to avoid duplication.
const LOCKFILE_MANAGERS: &[(&[&str], PackageManager)] = &[
    (&["bun.lockb", "bun.lock"], PackageManager::Bun),
    (&["pnpm-lock.yaml"], PackageManager::Pnpm),
    (&["yarn.lock"], PackageManager::Yarn),
];

impl PackageManager {
    /// Returns the publish command for this package manager.
    ///
    /// `PackageManager` is `#[derive(Clone, Copy)]`, so this takes `self`
    /// by value (not `&self`) — matching the sibling
    /// [`changepacks_core::Language::publish_key`] const accessor's
    /// `const fn (self)` shape. Making this `const fn` unlocks
    /// compile-time evaluation and is a pure code-quality gain: every
    /// call site already owns a `PackageManager` value on the stack.
    #[must_use]
    pub const fn publish_command(self) -> &'static str {
        match self {
            Self::Npm => "npm publish",
            Self::Yarn => "yarn npm publish",
            Self::Pnpm => "pnpm publish",
            Self::Bun => "bun publish",
        }
    }

    /// Returns the dry-run publish command for this package manager.
    ///
    /// All four supported managers natively support `--dry-run`, so this
    /// always returns `Some`. The flag placement matches each tool's CLI:
    /// - `npm publish --dry-run`
    /// - `yarn npm publish --dry-run`
    /// - `pnpm publish --dry-run`
    /// - `bun publish --dry-run`
    ///
    /// `PackageManager` is `#[derive(Clone, Copy)]`, so this takes `self`
    /// by value (not `&self`) — matching the sibling
    /// [`changepacks_core::Language::publish_key`] const accessor's
    /// `const fn (self)` shape. See [`Self::publish_command`] for the
    /// rationale.
    #[must_use]
    pub const fn dry_run_publish_command(self) -> &'static str {
        match self {
            Self::Npm => "npm publish --dry-run",
            Self::Yarn => "yarn npm publish --dry-run",
            Self::Pnpm => "pnpm publish --dry-run",
            Self::Bun => "bun publish --dry-run",
        }
    }
}

/// Collect the existing `node_modules/.bin` directories from `start_dir`
/// upward, nearest first.
///
/// The walk is BOUNDED to the repository root via `max_depth` — the number of
/// directories inspected, starting at `start_dir`. Callers pass
/// `relative_path.components().count()` (the count from the manifest's parent
/// dir up to and INCLUDING the repo root), the same convention as
/// `detect_package_manager_recursive`, so a `node_modules/.bin` ABOVE the git
/// root (e.g. one in the user's home dir) can no longer be prepended to the
/// publish child process's `PATH`.
///
/// npm, yarn, pnpm, and `bun install` all prepend these directories to `PATH`
/// when running package scripts, but `bun publish` / `bun pm pack` do not
/// (oven-sh/bun#16071, #18055, #23594). changepacks runs the publish command
/// itself, so it replicates that behaviour to keep `prepare` / `prepack`
/// hooks such as `husky` resolving during publish and dry-run.
///
/// Used by the publish / dry-run flow (`run_publish_for_path` /
/// `run_dry_run_publish_for_path`) to prepend `node_modules/.bin` to `PATH`
/// so lifecycle hooks such as `husky` resolve during `bun publish` / `bun pm
/// pack` (oven-sh/bun#16071, #18055, #23594).
pub async fn node_modules_bin_dirs_async(start_dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in start_dir.ancestors().take(max_depth) {
        let bin = dir.join("node_modules").join(".bin");
        if tokio::fs::metadata(&bin)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            dirs.push(bin);
        }
    }
    dirs
}

/// Detects the package manager by checking for lock files in the given directory
/// Priority: bun.lockb/bun.lock > pnpm-lock.yaml > yarn.lock > npm (default; a lone
/// package-lock.json is resolved by `detect_package_manager_recursive`)
#[must_use]
pub fn detect_package_manager(dir: &Path) -> PackageManager {
    for (lockfiles, pm) in LOCKFILE_MANAGERS {
        if lockfiles.iter().any(|f| dir.join(f).is_file()) {
            return *pm;
        }
    }
    // No bun/pnpm/yarn lockfile: default to npm. This also covers the
    // package-lock.json case (npm), so no separate stat is needed here —
    // detect_package_manager_recursive does its own package-lock.json check.
    PackageManager::Npm
}

/// Async equivalent of [`detect_package_manager`] for publish flows.
pub async fn detect_package_manager_async(dir: &Path) -> PackageManager {
    for (lockfiles, pm) in LOCKFILE_MANAGERS {
        for lockfile in *lockfiles {
            if is_regular_file(&dir.join(lockfile)).await {
                return *pm;
            }
        }
    }
    PackageManager::Npm
}

/// Detects the package manager by searching from the given path upward.
///
/// The walk is BOUNDED to the repository root via `max_depth` — the number of
/// directories inspected, starting at the manifest's directory. Callers pass
/// `relative_path.components().count()` (the count from the manifest's parent
/// dir up to and INCLUDING the repo root), so a stray lockfile ABOVE the git
/// root can no longer flip the detected package manager. Mirrors the git-scoped
/// `ancestors().take(max_depth)` bound the sibling Java (`find_gradlew`) and C#
/// (`is_workspace`) finders apply. "Nearest lockfile wins" still holds within
/// the bound: the first non-npm hit (or a `package-lock.json`) returns.
#[must_use]
pub fn detect_package_manager_recursive(path: &Path, max_depth: usize) -> PackageManager {
    let start = if path.is_file() {
        path.parent()
    } else {
        Some(path)
    };
    let Some(start) = start else {
        return PackageManager::Npm;
    };

    for dir in start.ancestors().take(max_depth) {
        let pm = detect_package_manager(dir);
        if pm != PackageManager::Npm || dir.join("package-lock.json").is_file() {
            return pm;
        }
    }

    PackageManager::Npm
}

/// Async equivalent of [`detect_package_manager_recursive`] for publish flows.
///
/// The walk is BOUNDED to the repository root via `max_depth` — the number of
/// directories inspected, starting at the manifest's directory (see the sync
/// twin for the full rationale). Kept in strict parity with the sync body.
pub async fn detect_package_manager_recursive_async(
    path: &Path,
    max_depth: usize,
) -> PackageManager {
    let start = if is_regular_file(path).await {
        path.parent()
    } else {
        Some(path)
    };
    let Some(start) = start else {
        return PackageManager::Npm;
    };

    for dir in start.ancestors().take(max_depth) {
        let pm = detect_package_manager_async(dir).await;
        if pm != PackageManager::Npm || is_regular_file(&dir.join("package-lock.json")).await {
            return pm;
        }
    }

    PackageManager::Npm
}

/// Shared helper for resolving publish commands by config or detected package manager.
///
/// Checks the provided config map first; if no match, detects the package manager
/// recursively and calls the provided accessor function to get the default command.
async fn command_for_path(
    path: &Path,
    relative_path: &Path,
    map: &std::collections::HashMap<String, String>,
    default_fn: fn(PackageManager) -> &'static str,
) -> String {
    if let Some(command) =
        changepacks_core::publish::lookup_by_path_or_language(map, relative_path, Language::Node)
    {
        return command;
    }
    default_fn(
        detect_package_manager_recursive_async(path, relative_path.components().count()).await,
    )
    .to_string()
}

pub(crate) async fn publish_command_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
) -> String {
    command_for_path(
        path,
        relative_path,
        &config.publish,
        PackageManager::publish_command,
    )
    .await
}

pub(crate) async fn dry_run_publish_command_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
) -> String {
    command_for_path(
        path,
        relative_path,
        &config.publish_dry_run,
        PackageManager::dry_run_publish_command,
    )
    .await
}

async fn publish_path_dirs_for_path(path: &Path, relative_path: &Path) -> Vec<PathBuf> {
    match path.parent() {
        Some(parent) => {
            node_modules_bin_dirs_async(parent, relative_path.components().count()).await
        }
        None => Vec::new(),
    }
}

pub(crate) async fn run_publish_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_message: &'static str,
) -> Result<changepacks_core::publish::PublishOutput> {
    let command = publish_command_for_path(path, relative_path, config).await;
    let path_dirs = publish_path_dirs_for_path(path, relative_path).await;
    changepacks_core::publish::run_publish_flow(&command, path, &path_dirs, missing_dir_message)
        .await
}

/// Run the dry-run publish command for a Node package.
///
/// Node package managers (npm, yarn, pnpm) always support `--dry-run`, so this
/// always returns `Ok(Some(output))` on success (never `Ok(None)`).
pub(crate) async fn run_dry_run_publish_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_message: &'static str,
) -> Result<Option<changepacks_core::publish::PublishOutput>> {
    let command = dry_run_publish_command_for_path(path, relative_path, config).await;
    let path_dirs = publish_path_dirs_for_path(path, relative_path).await;
    changepacks_core::publish::run_publish_flow(&command, path, &path_dirs, missing_dir_message)
        .await
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::UpdateType;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;

    // Lock-file → package-manager detection. Single-file cases exercise each
    // supported lock file (plus the "no file" default-npm fall-through); the
    // multi-file cases lock in the priority order documented on
    // `detect_package_manager` (bun > pnpm > yarn > npm).
    #[rstest]
    #[case(&["bun.lockb"], PackageManager::Bun)]
    #[case(&["bun.lock"], PackageManager::Bun)]
    #[case(&["pnpm-lock.yaml"], PackageManager::Pnpm)]
    #[case(&["yarn.lock"], PackageManager::Yarn)]
    #[case(&["package-lock.json"], PackageManager::Npm)]
    #[case(&[], PackageManager::Npm)]
    #[case(&["bun.lockb", "pnpm-lock.yaml", "yarn.lock"], PackageManager::Bun)]
    #[case(&["pnpm-lock.yaml", "yarn.lock"], PackageManager::Pnpm)]
    fn test_detect_package_manager(#[case] lock_files: &[&str], #[case] expected: PackageManager) {
        let temp_dir = TempDir::new().unwrap();
        for f in lock_files {
            fs::write(temp_dir.path().join(f), "").unwrap();
        }
        assert_eq!(detect_package_manager(temp_dir.path()), expected);
    }

    #[rstest]
    #[case(PackageManager::Npm, "npm publish")]
    #[case(PackageManager::Yarn, "yarn npm publish")]
    #[case(PackageManager::Pnpm, "pnpm publish")]
    #[case(PackageManager::Bun, "bun publish")]
    fn test_publish_commands(#[case] pm: PackageManager, #[case] expected: &str) {
        assert_eq!(pm.publish_command(), expected);
    }

    // All four supported package managers natively support `--dry-run`.
    // Source: official CLI docs for npm, pnpm, yarn berry (`yarn npm
    // publish -n/--dry-run`) and bun publish.
    #[rstest]
    #[case(PackageManager::Npm, "npm publish --dry-run")]
    #[case(PackageManager::Yarn, "yarn npm publish --dry-run")]
    #[case(PackageManager::Pnpm, "pnpm publish --dry-run")]
    #[case(PackageManager::Bun, "bun publish --dry-run")]
    fn test_dry_run_publish_commands(#[case] pm: PackageManager, #[case] expected: &str) {
        assert_eq!(pm.dry_run_publish_command(), expected);
    }

    #[test]
    fn test_detect_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let sub_dir = temp_dir.path().join("packages").join("core");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        fs::write(sub_dir.join("package.json"), "{}").unwrap();

        // Manifest `packages/core/package.json` is repo-root-relative with 3
        // components, so the walk scans `packages/core`, `packages`, then the
        // repo root — reaching the root `pnpm-lock.yaml`.
        assert_eq!(
            detect_package_manager_recursive(&sub_dir.join("package.json"), 3),
            PackageManager::Pnpm
        );
    }

    /// Regression: locks the "walk upward, stop at the FIRST non-npm hit"
    /// contract of `detect_package_manager_recursive`. When a nearer
    /// directory holds a non-npm lockfile and a further ancestor holds a
    /// DIFFERENT non-npm lockfile, the nearer one MUST win. `test_detect_recursive`
    /// only pins the single-lockfile case, so this test complements it
    /// with the two-lockfiles case a naive edit could silently break —
    /// e.g. a refactor that reversed the direction of the ancestor walk,
    /// or bubbled the outermost lock up instead of the innermost.
    ///
    /// Fixture: `<tmp>/bun.lock` at the root + `<tmp>/pkg/pnpm-lock.yaml`
    /// one level in, resolved from `<tmp>/pkg/sub/package.json` two
    /// levels deep. The nearer `pnpm-lock.yaml` must win over the
    /// outermost `bun.lock` — encoding "nearest lockfile beats ancestor".
    #[test]
    fn test_detect_recursive_nearer_lockfile_wins_over_ancestor() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let pkg_dir = root.join("pkg");
        let sub_dir = pkg_dir.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();

        // Ancestor lockfile (further from the manifest).
        fs::write(root.join("bun.lock"), "").unwrap();
        // Nearer lockfile (should win).
        fs::write(pkg_dir.join("pnpm-lock.yaml"), "").unwrap();
        fs::write(sub_dir.join("package.json"), "{}").unwrap();

        // Manifest `pkg/sub/package.json` is repo-root-relative with 3
        // components → depth 3: the walk scans `pkg/sub`, `pkg` (nearer
        // pnpm-lock.yaml), then the root (ancestor bun.lock).
        assert_eq!(
            detect_package_manager_recursive(&sub_dir.join("package.json"), 3),
            PackageManager::Pnpm,
            "expected the nearer pnpm-lock.yaml to beat the ancestor bun.lock"
        );
    }

    /// Regression: a decoy lockfile ABOVE the repository root must NOT flip the
    /// detected package manager. The recursive walk is now BOUNDED by
    /// `max_depth` (the caller passes `relative_path.components().count()`), so
    /// it scans only the manifest's in-repo ancestors — down to the repo root —
    /// and never reaches the out-of-repo directory holding the stray lockfile.
    /// Package-manager detection is git-scoped: a `pnpm-lock.yaml` in the user's
    /// home dir, the drive root, or a sibling checkout must not silently switch
    /// the publish command. Against the old unbounded walk (which climbed to the
    /// filesystem root) this decoy WOULD have returned Pnpm, so this test fails
    /// there and passes only once the walk is bounded. Complements
    /// `test_detect_recursive_nearer_lockfile_wins_over_ancestor`, which pins
    /// that an IN-repo ancestor lockfile is still honored.
    ///
    /// Fixture: `<tmp>/pnpm-lock.yaml` (decoy, ABOVE the repo root) +
    /// `<tmp>/repo/sub/package.json`. `relative_path` is repo-root-relative
    /// with 2 components (`sub/package.json`) → depth 2: the walk scans
    /// `<tmp>/repo/sub` and `<tmp>/repo` only — never `<tmp>`.
    #[test]
    fn test_detect_recursive_ignores_lockfile_above_repo_root() {
        let temp_dir = TempDir::new().unwrap();
        // Decoy lockfile ABOVE the repo root — must be ignored.
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();

        let repo_root = temp_dir.path().join("repo");
        let sub_dir = repo_root.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(sub_dir.join("package.json"), "{}").unwrap();

        assert_eq!(
            detect_package_manager_recursive(&sub_dir.join("package.json"), 2),
            PackageManager::Npm,
            "expected a decoy pnpm-lock.yaml above the repo root to be ignored"
        );
    }

    #[tokio::test]
    async fn test_node_modules_bin_dirs_collects_ancestors_nearest_first() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let root_bin = root.join("node_modules").join(".bin");
        fs::create_dir_all(&root_bin).unwrap();
        let pkg_dir = root.join("packages").join("app");
        let pkg_bin = pkg_dir.join("node_modules").join(".bin");
        fs::create_dir_all(&pkg_bin).unwrap();

        // depth 3 simulates the relative path `packages/app/package.json`
        // (3 components), covering `packages/app` .. `root` inclusive.
        let dirs = node_modules_bin_dirs_async(&pkg_dir, 3).await;
        // Nearest (package-level) bin dir comes first, ancestor (root) after.
        assert_eq!(dirs.first(), Some(&pkg_bin));
        assert!(dirs.contains(&root_bin));
    }

    /// Decoy: a `node_modules/.bin` ABOVE the repo root must never reach the
    /// publish child process's `PATH`. Mirrors
    /// `test_detect_recursive_ignores_lockfile_above_repo_root`.
    ///
    /// Fixture: `<tmp>/outer/node_modules/.bin` (decoy, ABOVE the repo root) +
    /// repo root `<tmp>/outer/repo`, package dir `<tmp>/outer/repo/pkg` (with
    /// its own `node_modules/.bin`). `relative_path` is repo-root-relative with
    /// 2 components (`pkg/package.json`) → depth 2: the walk scans
    /// `<tmp>/outer/repo/pkg` and `<tmp>/outer/repo` only — never `<tmp>/outer`.
    #[tokio::test]
    async fn test_node_modules_bin_dirs_ignores_bin_above_repo_root() {
        let temp_dir = TempDir::new().unwrap();
        let outer = temp_dir.path().join("outer");
        // Decoy bin ABOVE the repo root — must be ignored.
        let outer_bin = outer.join("node_modules").join(".bin");
        fs::create_dir_all(&outer_bin).unwrap();

        let repo_root = outer.join("repo");
        let pkg_dir = repo_root.join("pkg");
        let pkg_bin = pkg_dir.join("node_modules").join(".bin");
        fs::create_dir_all(&pkg_bin).unwrap();

        // depth 2 == `pkg/package.json`.components().count(): scans `pkg` and
        // the repo root, never the parent that holds the decoy bin.
        let dirs = node_modules_bin_dirs_async(&pkg_dir, 2).await;
        assert!(
            !dirs.contains(&outer_bin),
            "expected a node_modules/.bin above the repo root to be ignored"
        );
        // The package's own bin is still collected, nearest-first.
        assert_eq!(dirs.first(), Some(&pkg_bin));
    }

    #[tokio::test]
    async fn test_node_modules_bin_dirs_empty_when_absent() {
        let temp_dir = TempDir::new().unwrap();
        // depth 1 inspects only the package dir itself — no bin here, and the
        // bound keeps a stray ancestor `node_modules/.bin` from leaking in.
        assert!(
            node_modules_bin_dirs_async(temp_dir.path(), 1)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_injected_path_resolves_bare_binary() {
        // End-to-end reproduction of the husky failure: a bare command name is
        // only resolvable because `node_modules/.bin` was prepended to PATH.
        // Without the injection, `bun publish` reports "husky: command not
        // found" (exit 127); this asserts changepacks' injection fixes it.
        let temp_dir = TempDir::new().unwrap();
        let bin = temp_dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin).unwrap();

        let hook = bin.join(if cfg!(target_os = "windows") {
            "cphook.cmd"
        } else {
            "cphook"
        });
        if cfg!(target_os = "windows") {
            fs::write(&hook, "@echo hook-ran\r\n").unwrap();
        } else {
            fs::write(&hook, "#!/bin/sh\necho hook-ran\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        // depth 1 covers the package dir, where `node_modules/.bin` lives.
        let dirs = node_modules_bin_dirs_async(temp_dir.path(), 1).await;
        assert!(dirs.contains(&bin));

        // Bare command name; resolvable only via the injected PATH entry.
        let output = changepacks_core::publish::run_publish_command_with_path_dirs(
            "cphook",
            temp_dir.path(),
            &dirs,
        )
        .await
        .unwrap();
        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("hook-ran"));
    }

    /// Regression: a failed `package.json` WRITE must name the manifest path
    /// in the error chain — locking the `.with_context(...)` added to
    /// `write_package_json_version` so a write failure reads as clearly as the
    /// read/parse contexts already in this file. Marks the file readonly (the
    /// cross-platform lever that also denies the write-open on Windows, where
    /// this suite runs) so the write is REJECTED, then asserts the formatted
    /// error chain contains the manifest path.
    #[tokio::test]
    async fn test_write_package_json_version_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(&package_json, "{\n  \"version\": \"1.0.0\"\n}\n").unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        let mut permissions = fs::metadata(&package_json).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&package_json, permissions).unwrap();

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result = write_package_json_version(&package_json, "2.0.0").await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        let mut permissions = fs::metadata(&package_json).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&package_json, permissions).unwrap();

        let err = result.expect_err("write to a readonly package.json must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&package_json.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }

    /// Regression: a non-object root in `package.json` must fail with an error
    /// that names the manifest path, rather than panicking via serde_json's
    /// IndexMut. A `[]` root is valid JSON but degenerate — the error must
    /// clearly indicate the problem and include the file path.
    #[tokio::test]
    async fn test_write_package_json_version_errors_on_non_object_root() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(&package_json, "[]").unwrap();

        let result = write_package_json_version(&package_json, "1.0.0").await;

        let err = result.expect_err("non-object root must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&package_json.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("does not have a top-level JSON object"),
            "error chain should describe the problem, got: {chain}"
        );
    }

    // Async parity tests: mirror the sync `test_detect_package_manager` rstest
    // table to ensure async and sync detection remain in sync. A drift between
    // them would ship unnoticed without these tests.
    #[rstest]
    #[case(&["bun.lockb"], PackageManager::Bun)]
    #[case(&["bun.lock"], PackageManager::Bun)]
    #[case(&["pnpm-lock.yaml"], PackageManager::Pnpm)]
    #[case(&["yarn.lock"], PackageManager::Yarn)]
    #[case(&["package-lock.json"], PackageManager::Npm)]
    #[case(&[], PackageManager::Npm)]
    #[case(&["bun.lockb", "pnpm-lock.yaml", "yarn.lock"], PackageManager::Bun)]
    #[case(&["pnpm-lock.yaml", "yarn.lock"], PackageManager::Pnpm)]
    #[tokio::test]
    async fn test_detect_package_manager_async(
        #[case] lock_files: &[&str],
        #[case] expected: PackageManager,
    ) {
        let temp_dir = TempDir::new().unwrap();
        for f in lock_files {
            fs::write(temp_dir.path().join(f), "").unwrap();
        }
        assert_eq!(
            detect_package_manager_async(temp_dir.path()).await,
            expected
        );
    }

    /// Async parity test: mirrors `test_detect_recursive_nearer_lockfile_wins_over_ancestor`
    /// to ensure async and sync recursive detection remain in sync. Regression lock:
    /// the "walk upward, stop at the FIRST non-npm hit" contract must hold for async.
    #[tokio::test]
    async fn test_detect_package_manager_recursive_async_nearer_lockfile_wins_over_ancestor() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let pkg_dir = root.join("pkg");
        let sub_dir = pkg_dir.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();

        // Ancestor lockfile (further from the manifest).
        fs::write(root.join("bun.lock"), "").unwrap();
        // Nearer lockfile (should win).
        fs::write(pkg_dir.join("pnpm-lock.yaml"), "").unwrap();
        fs::write(sub_dir.join("package.json"), "{}").unwrap();

        // Manifest `pkg/sub/package.json` is repo-root-relative with 3
        // components → depth 3: the walk scans `pkg/sub`, `pkg` (nearer
        // pnpm-lock.yaml), then the root (ancestor bun.lock).
        assert_eq!(
            detect_package_manager_recursive_async(&sub_dir.join("package.json"), 3).await,
            PackageManager::Pnpm,
            "expected the nearer pnpm-lock.yaml to beat the ancestor bun.lock"
        );
    }

    /// Async parity test: mirrors `test_detect_recursive_ignores_lockfile_above_repo_root`
    /// to ensure async and sync recursive detection remain in sync. Regression
    /// lock: a decoy lockfile ABOVE the repo root must not flip the async
    /// detector's result now that its walk is bounded by `max_depth`. By
    /// construction the old unbounded walk would have returned Pnpm.
    #[tokio::test]
    async fn test_detect_package_manager_recursive_async_ignores_lockfile_above_repo_root() {
        let temp_dir = TempDir::new().unwrap();
        // Decoy lockfile ABOVE the repo root — must be ignored.
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();

        let repo_root = temp_dir.path().join("repo");
        let sub_dir = repo_root.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(sub_dir.join("package.json"), "{}").unwrap();

        // `sub/package.json` → 2 components → depth 2: the walk scans
        // `<tmp>/repo/sub` and `<tmp>/repo` only — never `<tmp>`.
        assert_eq!(
            detect_package_manager_recursive_async(&sub_dir.join("package.json"), 2).await,
            PackageManager::Npm,
            "expected a decoy pnpm-lock.yaml above the repo root to be ignored"
        );
    }

    /// Regression: a malformed version must fail the bump and name the
    /// manifest in the error chain — completing the path-in-error-context
    /// pattern the read/parse/write helpers in this file already use. The bump
    /// errors BEFORE any file I/O, so no on-disk fixture is needed.
    #[tokio::test]
    async fn test_bump_version_with_bump_error_includes_path() {
        let manifest = PathBuf::from("/nonexistent/node-bump/package.json");
        let mut version = Some("abc".to_string());
        let err = changepacks_utils::bump_version_with(
            &mut version,
            &manifest,
            UpdateType::Patch,
            async |_| Ok(()),
        )
        .await
        .expect_err("a malformed version must fail the bump");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&manifest.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }

    // ── publish_command_for_path / dry_run_publish_command_for_path precedence ──
    //
    // The resolution ladder (highest → lowest priority):
    //   1. per-path entry in config.publish / config.publish_dry_run
    //   2. per-language "node" entry in the same map
    //   3. detected package manager (lockfile walk) → default command string
    //
    // Tests (a)–(d) below pin each rung of that ladder for both the publish
    // and dry-run variants, mirroring the tempdir/lockfile fixture style used
    // by the existing tests above.

    /// (a) Per-path entry in `config.publish` wins over everything else,
    /// including a `pnpm-lock.yaml` that would otherwise select pnpm.
    /// The dry-run variant is covered by the separate `publish_dry_run` map.
    #[tokio::test]
    async fn test_publish_command_per_path_wins() {
        let temp_dir = TempDir::new().unwrap();
        // A pnpm lockfile is present — without the per-path override it would
        // resolve to "pnpm publish".
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        // relative_path key must match what lookup_by_path_or_language uses:
        // relative_path.to_string_lossy().
        let relative_path = PathBuf::from("package.json");
        let mut config = Config::default();
        config
            .publish
            .insert("package.json".to_string(), "custom-publish".to_string());

        let cmd = publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "custom-publish");
    }

    /// (a-dry) Per-path entry in `config.publish_dry_run` wins over the
    /// lockfile-detected default dry-run command.
    #[tokio::test]
    async fn test_dry_run_publish_command_per_path_wins() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        let relative_path = PathBuf::from("package.json");
        let mut config = Config::default();
        config
            .publish_dry_run
            .insert("package.json".to_string(), "custom-dry-run".to_string());

        let cmd = dry_run_publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "custom-dry-run");
    }

    /// (b) Per-language "node" entry in `config.publish` wins when no per-path
    /// entry exists, even when a lockfile would otherwise select a different
    /// package manager.
    #[tokio::test]
    async fn test_publish_command_per_language_wins_over_lockfile() {
        let temp_dir = TempDir::new().unwrap();
        // bun.lock would normally resolve to "bun publish".
        fs::write(temp_dir.path().join("bun.lock"), "").unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        let relative_path = PathBuf::from("package.json");
        let mut config = Config::default();
        config
            .publish
            .insert("node".to_string(), "node-lang-publish".to_string());

        let cmd = publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "node-lang-publish");
    }

    /// (b-dry) Per-language "node" entry in `config.publish_dry_run` wins
    /// when no per-path entry exists.
    #[tokio::test]
    async fn test_dry_run_publish_command_per_language_wins_over_lockfile() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("bun.lock"), "").unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        let relative_path = PathBuf::from("package.json");
        let mut config = Config::default();
        config
            .publish_dry_run
            .insert("node".to_string(), "node-lang-dry-run".to_string());

        let cmd = dry_run_publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "node-lang-dry-run");
    }

    /// (b-precedence) Per-path entry beats the per-language "node" entry when
    /// both are present in the same config map.
    #[tokio::test]
    async fn test_publish_command_per_path_beats_per_language() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        let relative_path = PathBuf::from("package.json");
        let mut config = Config::default();
        config
            .publish
            .insert("node".to_string(), "node-lang-publish".to_string());
        config
            .publish
            .insert("package.json".to_string(), "path-publish".to_string());

        let cmd = publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "path-publish");
    }

    /// (c) Empty config + `pnpm-lock.yaml` in the tempdir → `"pnpm publish"`.
    #[tokio::test]
    async fn test_publish_command_detects_pnpm_from_lockfile() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        // relative_path with 1 component → depth 1: only the manifest's own
        // directory is scanned, which is where pnpm-lock.yaml lives.
        let relative_path = PathBuf::from("package.json");
        let config = Config::default();

        let cmd = publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "pnpm publish");
    }

    /// (c-dry) Empty config + `pnpm-lock.yaml` → `"pnpm publish --dry-run"`.
    #[tokio::test]
    async fn test_dry_run_publish_command_detects_pnpm_from_lockfile() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        let relative_path = PathBuf::from("package.json");
        let config = Config::default();

        let cmd = dry_run_publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "pnpm publish --dry-run");
    }

    /// (d) Empty config + no lockfile in tempdir → `"npm publish"` (default).
    #[tokio::test]
    async fn test_publish_command_defaults_to_npm_when_no_lockfile() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        let relative_path = PathBuf::from("package.json");
        let config = Config::default();

        let cmd = publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "npm publish");
    }

    /// (d-dry) Empty config + no lockfile → `"npm publish --dry-run"`.
    #[tokio::test]
    async fn test_dry_run_publish_command_defaults_to_npm_when_no_lockfile() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("package.json");
        fs::write(&manifest, "{}").unwrap();

        let relative_path = PathBuf::from("package.json");
        let config = Config::default();

        let cmd = dry_run_publish_command_for_path(&manifest, &relative_path, &config).await;
        assert_eq!(cmd, "npm publish --dry-run");
    }
}
