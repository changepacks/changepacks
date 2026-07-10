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
use changepacks_core::{Config, Language, UpdateType, is_regular_file};
use changepacks_utils::{detect_indent_str, finalize_content, next_version_or_default};
use serde::Serialize;
use tokio::fs::{read_to_string, write};

/// Shared body for `NodePackage::update_version` and
/// `NodeWorkspace::update_version`.
///
/// Consolidates the "read current version, compute next, write
/// package.json, stash new version on `self`" 5-line sequence —
/// previously duplicated (semantically identical) between `NodePackage`
/// and `NodeWorkspace` — into ONE source of truth. Both trait impls now
/// delegate here so a future rewording of the "reserve `0.0.0` when
/// unversioned" fallback lands in exactly one place.
///
/// A shared helper (rather than a `macro_rules!` mirroring
/// `impl_node_publish_wiring!()`) is required because `#[async_trait]`
/// runs BEFORE declarative-macro expansion — see the twin helper in
/// `crates/dart/src/lib.rs` for the full E0195 rationale.
///
/// # Errors
/// Returns error if the version update or package.json write fails.
pub(crate) async fn update_version_from_fields(
    version: &mut Option<String>,
    path: &Path,
    update_type: UpdateType,
) -> Result<()> {
    // Two-line "reserve `0.0.0` when unversioned" prelude consolidated
    // into `changepacks_utils::next_version_or_default` so the fallback
    // policy lives in ONE place across every language crate. See that
    // helper's doc for the Java/Rust carve-outs.
    let new_version = next_version_or_default(version.as_deref(), update_type)?;
    write_package_json_version(path, &new_version).await?;
    *version = Some(new_version);
    Ok(())
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
    let package_json_raw = read_to_string(path)
        .await
        .with_context(|| format!("Failed to read package.json {}", path.display()))?;
    let indent_str = detect_indent_str(&package_json_raw);
    let mut package_json: serde_json::Value = serde_json::from_str(&package_json_raw)
        .with_context(|| format!("Failed to parse package.json {}", path.display()))?;
    package_json["version"] = serde_json::Value::String(new_version.to_string());
    let ind = indent_str.as_bytes();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(ind);
    let writer = Vec::with_capacity(package_json_raw.len());
    let mut ser = serde_json::Serializer::with_formatter(writer, formatter);
    package_json.serialize(&mut ser)?;
    write(
        path,
        finalize_content(&String::from_utf8(ser.into_inner())?, &package_json_raw),
    )
    .await?;
    Ok(())
}

/// Expand to the identical `default_publish_command` /
/// `default_dry_run_publish_command` / `publish_path_dirs` triple used by
/// both `NodePackage` and `NodeWorkspace`.
///
/// Node cannot use `changepacks_core::impl_const_publish_commands!()`
/// because its publish command is determined at runtime by walking the
/// ancestor chain via `detect_package_manager_recursive`, not from a
/// compile-time const. It also needs the `publish_path_dirs` override so
/// `node_modules/.bin` gets prepended to `PATH` (workaround for
/// oven-sh/bun#16071, #18055, #23594 — bun does not add
/// `node_modules/.bin` to `PATH` during `bun publish` / `bun pm pack`).
///
/// Invoked from inside an `impl Package for NodePackage` or `impl
/// Workspace for NodeWorkspace` block. Byte-identical expansion — the
/// previously hand-rolled bodies:
///
/// ```ignore
/// fn default_publish_command(&self) -> String {
///     detect_package_manager_recursive(&self.path).publish_command().to_string()
/// }
/// fn default_dry_run_publish_command(&self) -> Option<String> {
///     Some(detect_package_manager_recursive(&self.path).dry_run_publish_command().to_string())
/// }
/// fn publish_path_dirs(&self) -> Vec<PathBuf> {
///     self.path.parent().map(node_modules_bin_dirs).unwrap_or_default()
/// }
/// ```
///
/// are replaced 1:1 by a single `crate::impl_node_publish_wiring!();`
/// invocation. Fully-qualified `::std::string::String`,
/// `::std::option::Option`, `::std::vec::Vec`, and
/// `::std::path::PathBuf` make the macro hygienic — callers do not need
/// those types in scope at the invocation site.
///
/// Consumer requirement: the struct must have a `path: PathBuf` field
/// with that exact spelling. Both `NodePackage` and `NodeWorkspace`
/// satisfy this — the only two intended callers.
macro_rules! impl_node_publish_wiring {
    () => {
        fn default_publish_command(&self) -> ::std::string::String {
            $crate::detect_package_manager_recursive(&self.path)
                .publish_command()
                .to_string()
        }
        fn default_dry_run_publish_command(&self) -> ::std::option::Option<::std::string::String> {
            ::std::option::Option::Some(
                $crate::detect_package_manager_recursive(&self.path)
                    .dry_run_publish_command()
                    .to_string(),
            )
        }
        fn publish_path_dirs(&self) -> ::std::vec::Vec<::std::path::PathBuf> {
            self.path
                .parent()
                .map($crate::node_modules_bin_dirs)
                .unwrap_or_default()
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

/// Collect `node_modules/.bin` directories from `start_dir` up to the
/// filesystem root, nearest first.
///
/// npm, yarn, pnpm, and `bun install` all prepend these directories to `PATH`
/// when running package scripts, but `bun publish` / `bun pm pack` do not
/// (oven-sh/bun#16071, #18055, #23594). changepacks runs the publish command
/// itself, so it replicates that behaviour to keep `prepare` / `prepack`
/// hooks such as `husky` resolving during publish and dry-run.
#[must_use]
fn node_modules_bin_candidates(start_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(start_dir.ancestors().count());
    dirs.extend(
        start_dir
            .ancestors()
            .map(|dir| dir.join("node_modules").join(".bin")),
    );
    dirs
}

#[must_use]
pub fn node_modules_bin_dirs(start_dir: &Path) -> Vec<PathBuf> {
    node_modules_bin_candidates(start_dir)
        .into_iter()
        .filter(|bin| bin.is_dir())
        .collect()
}

/// Async equivalent of [`node_modules_bin_dirs`] for publish flows that are
/// already running inside Tokio.
pub async fn node_modules_bin_dirs_async(start_dir: &Path) -> Vec<PathBuf> {
    let candidates = node_modules_bin_candidates(start_dir);
    let mut dirs = Vec::with_capacity(candidates.len());
    for bin in candidates {
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

/// Detects the package manager by searching from the given path up to the root
#[must_use]
pub fn detect_package_manager_recursive(path: &Path) -> PackageManager {
    let mut current = if path.is_file() {
        path.parent()
    } else {
        Some(path)
    };

    while let Some(dir) = current {
        let pm = detect_package_manager(dir);
        if pm != PackageManager::Npm || dir.join("package-lock.json").is_file() {
            return pm;
        }
        current = dir.parent();
    }

    PackageManager::Npm
}

/// Async equivalent of [`detect_package_manager_recursive`] for publish flows.
pub async fn detect_package_manager_recursive_async(path: &Path) -> PackageManager {
    let mut current = if is_regular_file(path).await {
        path.parent()
    } else {
        Some(path)
    };

    while let Some(dir) = current {
        let pm = detect_package_manager_async(dir).await;
        if pm != PackageManager::Npm || is_regular_file(&dir.join("package-lock.json")).await {
            return pm;
        }
        current = dir.parent();
    }

    PackageManager::Npm
}

fn config_command(
    map: &std::collections::HashMap<String, String>,
    relative_path: &Path,
) -> Option<String> {
    changepacks_core::publish::lookup_by_path_or_language(map, relative_path, Language::Node)
}

pub(crate) async fn publish_command_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
) -> String {
    if let Some(command) = config_command(&config.publish, relative_path) {
        return command;
    }
    detect_package_manager_recursive_async(path)
        .await
        .publish_command()
        .to_string()
}

pub(crate) async fn dry_run_publish_command_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
) -> Option<String> {
    if let Some(command) = config_command(&config.publish_dry_run, relative_path) {
        return Some(command);
    }
    Some(
        detect_package_manager_recursive_async(path)
            .await
            .dry_run_publish_command()
            .to_string(),
    )
}

async fn publish_path_dirs_for_path(path: &Path) -> Vec<PathBuf> {
    match path.parent() {
        Some(parent) => node_modules_bin_dirs_async(parent).await,
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
    let path_dirs = publish_path_dirs_for_path(path).await;
    changepacks_core::publish::run_publish_flow(&command, path, &path_dirs, missing_dir_message)
        .await
}

pub(crate) async fn run_dry_run_publish_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_message: &'static str,
) -> Result<Option<changepacks_core::publish::PublishOutput>> {
    let command = dry_run_publish_command_for_path(path, relative_path, config).await;
    let path_dirs = publish_path_dirs_for_path(path).await;
    changepacks_core::publish::run_dry_run_publish_flow(
        command.as_deref(),
        path,
        &path_dirs,
        missing_dir_message,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
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

        assert_eq!(
            detect_package_manager_recursive(&sub_dir.join("package.json")),
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

        assert_eq!(
            detect_package_manager_recursive(&sub_dir.join("package.json")),
            PackageManager::Pnpm,
            "expected the nearer pnpm-lock.yaml to beat the ancestor bun.lock"
        );
    }

    #[test]
    fn test_node_modules_bin_dirs_collects_ancestors_nearest_first() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let root_bin = root.join("node_modules").join(".bin");
        fs::create_dir_all(&root_bin).unwrap();
        let pkg_dir = root.join("packages").join("app");
        let pkg_bin = pkg_dir.join("node_modules").join(".bin");
        fs::create_dir_all(&pkg_bin).unwrap();

        let dirs = node_modules_bin_dirs(&pkg_dir);
        // Nearest (package-level) bin dir comes first, ancestor (root) after.
        assert_eq!(dirs.first(), Some(&pkg_bin));
        assert!(dirs.contains(&root_bin));
    }

    #[test]
    fn test_node_modules_bin_dirs_empty_when_absent() {
        let temp_dir = TempDir::new().unwrap();
        assert!(node_modules_bin_dirs(temp_dir.path()).is_empty());
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

        let dirs = node_modules_bin_dirs(temp_dir.path());
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
}
