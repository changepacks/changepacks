//! Shared hermetic git test fixtures.
//!
//! This module plays a dual role: it is compiled for this crate's own unit
//! tests via `#[cfg(test)]`, and it is exported to sibling crates' test suites
//! (e.g. `changepacks-cli`'s integration tests) via the `test-support` feature.
//! The feature path deliberately pulls in **no** dev-dependencies — the exported
//! helpers ([`DirGuard`], [`run_git`], [`init_git_repo`],
//! [`git_add_and_commit`], [`discover_repo`]) only touch `std` and `gix` (a
//! production dependency), so they compile in a plain (non-dev) build enabled
//! solely by the feature.
//!
//! `create_project` stays test-only (`#[cfg(test)]`, `pub(crate)`) because it
//! needs the `changepacks-node` dev-dependency, which is unavailable on the
//! feature path.

use std::path::{Path, PathBuf};

#[cfg(test)]
use changepacks_core::{Package, Project};
#[cfg(test)]
use changepacks_node::package::NodePackage;

/// Restore the original process working directory when dropped.
///
/// Tests that use this guard must run serially because the process working
/// directory is shared by all threads.
#[must_use = "the guard must be held to restore the original working directory"]
pub struct DirGuard {
    original: PathBuf,
}

impl DirGuard {
    /// Change the process working directory to `path` until the guard is dropped.
    ///
    /// # Panics
    ///
    /// Panics if the current working directory cannot be read or changed.
    pub fn change_to(path: &Path) -> Self {
        let original = std::env::current_dir()
            .unwrap_or_else(|err| panic!("failed to read the current working directory: {err}"));
        std::env::set_current_dir(path).unwrap_or_else(|err| {
            panic!(
                "failed to change the working directory to {}: {err}",
                path.display()
            )
        });
        Self { original }
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

/// Create a test project (a `NodePackage` at version `1.0.0`) with the given
/// dependencies.
///
/// Dependencies are stored as *names* (e.g. `"p2"`), matching every real
/// finder — `add_dependency` records the raw name, never a relative path — so
/// `sort_by_dependencies` resolves them by name via `name_to_index` and
/// `apply_reverse_dependencies` resolves them via `reverse_deps`.
#[cfg(test)]
pub(crate) fn create_project(name: &str, dependencies: Vec<&str>) -> Project {
    let mut package = NodePackage::new(
        Some(name.to_string()),
        Some("1.0.0".to_string()),
        PathBuf::from(format!("/test/{name}/package.json")),
        PathBuf::from(format!("{name}/package.json")),
    );
    for dep in dependencies {
        package.add_dependency(dep);
    }
    Project::Package(Box::new(package))
}

/// Create a test project (a `NodePackage` at version `1.0.0`) with no
/// dependencies, at a caller-chosen relative manifest path.
///
/// Complements [`create_project`], which derives the manifest path from the
/// name: this one fixes the *path* instead, for the tests that assert on
/// manifest paths (ambiguity diagnostics, path ordering) and therefore need
/// several projects to share one name at different paths, or a nameless
/// project at a known path. The absolute path is the relative one rooted at
/// `/test`, matching [`create_project`].
///
/// Tests whose relative path is not valid UTF-8 (the lossy-collision cases)
/// cannot express it as a `&str` and keep constructing `NodePackage` inline.
#[cfg(test)]
pub(crate) fn create_project_at(name: Option<&str>, relative_path: &str) -> Project {
    Project::Package(Box::new(NodePackage::new(
        name.map(str::to_string),
        Some("1.0.0".to_string()),
        PathBuf::from("/test").join(relative_path),
        PathBuf::from(relative_path),
    )))
}

/// Run `git <args>` in `path` under hermetic config, asserting the command
/// succeeded. Shared by [`init_git_repo`] and [`git_add_and_commit`] and by the
/// test modules that drive git-backed discovery.
///
/// # Panics
///
/// Panics if the `git` process cannot be spawned, or if `git` exits with a
/// non-zero status.
pub fn run_git(path: &Path, args: &[&str]) {
    // Prepend hermetic config so these fixtures never depend on the developer's
    // global git config. In particular `commit.gpgsign=true` would make `git
    // commit` block on a GPG passphrase (hanging the test) or fail; disabling it
    // keeps commits non-interactive. Asserting `status.success()` also turns a
    // silent git failure (which would otherwise leave the repo with no `main`
    // branch and break downstream ref lookups) into a loud, actionable panic.
    let output = std::process::Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .current_dir(path)
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn `git {}`: {err}", args.join(" ")));
    assert!(
        output.status.success(),
        "`git {}` failed ({}):\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Initialize a hermetic git repository at `path` for tests that exercise
/// git-backed discovery.
///
/// Pins the initial branch to `main` (`git init -b main`) and sets a local
/// `user.name` / `user.email` so commits never fall back to — or require — the
/// developer's global identity. This is the single source of truth for the git
/// init incantation this crate's unit-test modules would otherwise open-code
/// identically.
///
/// # Panics
///
/// Panics (via [`run_git`]) if any of the underlying `git` commands fail.
pub fn init_git_repo(path: &Path) {
    run_git(path, &["init", "-b", "main"]);
    run_git(path, &["config", "user.email", "test@test.com"]);
    run_git(path, &["config", "user.name", "Test"]);
}

/// Stage all changes and commit them with `message` in the repo at `path`.
///
/// # Panics
///
/// Panics (via [`run_git`]) if the underlying `git add` or `git commit` fails.
pub fn git_add_and_commit(path: &Path, message: &str) {
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", message]);
}

/// Discover a git repository at `path` and return a thread-safe handle.
///
/// Centralizes the `gix::discover(path).unwrap().into_sync()` pattern used
/// throughout the test modules to avoid open-coding this repeatedly.
///
/// # Panics
///
/// Panics if no git repository can be discovered at `path`.
pub fn discover_repo(path: &Path) -> gix::ThreadSafeRepository {
    gix::discover(path).expect("discover test repo").into_sync()
}

/// Asserts that a rejected `update_version` bump surfaced the manifest parse
/// failure and left the manifest byte-identical on disk.
///
/// Seven `update_version` tests — `NodePackage`, `NodeWorkspace`,
/// `PythonPackage`, `PythonWorkspace`, `DartPackage`, `DartWorkspace` and
/// `RustPackage` — ended with the same assertion tail, differing only in the
/// manifest label (`package.json` / `pyproject.toml` / `pubspec.yaml` /
/// `Cargo.toml`). The tail pins the
/// `Failed to <verb> <label> <path>` context template owned by
/// [`crate::read_and_parse`], so the assertion about that template belongs in
/// one place too, next to the helper that produces it.
///
/// Deliberately covers only the tail: each call site keeps its own
/// `fs::write`, its own constructor, and its own
/// `update_version(UpdateType::Patch).await`, and passes just the resulting
/// `Result` in. The setup is what actually differs per language, so hiding it
/// would cost more readability than it saves.
///
/// Contract: `$result` is the awaited `Result` from `update_version`, `$path`
/// is anything that borrows as a [`Path`] pointing at the manifest, `$label`
/// is the human-facing manifest name (`"package.json"`), and `$original` is
/// the exact string written to the manifest before the bump.
#[macro_export]
macro_rules! assert_malformed_manifest_rejected {
    ($result:expr, $path:expr, $label:expr, $original:expr) => {{
        let manifest_path: &::std::path::Path = ::std::convert::AsRef::as_ref($path);
        let manifest_label: &::std::primitive::str = $label;

        let err = $result.expect_err(&::std::format!(
            "a malformed {manifest_label} must fail the bump"
        ));
        let chain = ::std::format!("{err:#}");
        ::std::assert!(
            chain.contains(&::std::format!("Failed to parse {manifest_label}")),
            "error chain should name the parse failure, got: {chain}"
        );
        ::std::assert!(
            chain.contains(&manifest_path.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );

        // Byte-for-byte: an unparseable manifest must never be rewritten.
        ::std::assert_eq!(
            ::std::fs::read(manifest_path).unwrap_or_else(|err| ::std::panic!(
                "failed to re-read {}: {err}",
                manifest_path.display()
            )),
            ::std::primitive::str::as_bytes($original),
            "a rejected bump must leave the manifest byte-identical"
        );
    }};
}

/// Set the readonly permission on a file for testing permission-denied scenarios.
///
/// Reads the file's metadata, sets the readonly bit to the specified value, and
/// applies the updated permissions. This is a shared fixture for tests that verify
/// error handling when writes fail due to permission restrictions.
///
/// # Panics
///
/// Panics if metadata cannot be read or permissions cannot be set.
pub fn set_readonly(path: &Path, readonly: bool) {
    let mut permissions = std::fs::metadata(path)
        .unwrap_or_else(|err| panic!("failed to read metadata for {}: {err}", path.display()))
        .permissions();
    permissions.set_readonly(readonly);
    std::fs::set_permissions(path, permissions)
        .unwrap_or_else(|err| panic!("failed to set permissions for {}: {err}", path.display()));
}
