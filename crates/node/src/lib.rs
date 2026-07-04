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

use anyhow::Result;
use changepacks_utils::{detect_indent_str, trailing_newline};
use serde::Serialize;
use tokio::fs::{read_to_string, write};

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
    let package_json_raw = read_to_string(path).await?;
    let indent_str = detect_indent_str(&package_json_raw);
    let mut package_json: serde_json::Value = serde_json::from_str(&package_json_raw)?;
    package_json["version"] = serde_json::Value::String(new_version.to_string());
    let ind = indent_str.as_bytes();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(ind);
    let writer = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(writer, formatter);
    package_json.serialize(&mut ser)?;
    write(
        path,
        format!(
            "{}{}",
            String::from_utf8(ser.into_inner())?.trim_end(),
            trailing_newline(&package_json_raw)
        ),
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
#[macro_export]
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

/// Represents the detected Node.js package manager
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Bun,
}

impl PackageManager {
    /// Returns the publish command for this package manager
    #[must_use]
    pub fn publish_command(&self) -> &'static str {
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
    #[must_use]
    pub fn dry_run_publish_command(&self) -> &'static str {
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
pub fn node_modules_bin_dirs(start_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = Some(start_dir);
    while let Some(dir) = current {
        let bin = dir.join("node_modules").join(".bin");
        if bin.is_dir() {
            dirs.push(bin);
        }
        current = dir.parent();
    }
    dirs
}

/// Detects the package manager by checking for lock files in the given directory
/// Priority: bun.lockb > pnpm-lock.yaml > yarn.lock > package-lock.json > npm (default)
#[must_use]
pub fn detect_package_manager(dir: &Path) -> PackageManager {
    if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        PackageManager::Bun
    } else if dir.join("pnpm-lock.yaml").exists() {
        PackageManager::Pnpm
    } else if dir.join("yarn.lock").exists() {
        PackageManager::Yarn
    } else if dir.join("package-lock.json").exists() {
        PackageManager::Npm
    } else {
        // Default to npm if no lock file found
        PackageManager::Npm
    }
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
        if pm != PackageManager::Npm || dir.join("package-lock.json").exists() {
            return pm;
        }
        current = dir.parent();
    }

    PackageManager::Npm
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
