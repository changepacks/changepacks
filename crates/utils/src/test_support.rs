//! Shared hermetic git test fixtures.
//!
//! This module plays a dual role: it is compiled for this crate's own unit
//! tests via `#[cfg(test)]`, and it is exported to sibling crates' test suites
//! (e.g. `changepacks-cli`'s integration tests) via the `test-support` feature.
//! The feature path deliberately pulls in **no** dev-dependencies — the exported
//! helpers ([`run_git`], [`init_git_repo`], [`git_add_and_commit`],
//! [`discover_repo`]) only touch `std` and `gix` (a production dependency), so
//! they compile in a plain (non-dev) build enabled solely by the feature.
//!
//! `create_project` stays test-only (`#[cfg(test)]`, `pub(crate)`) because it
//! needs the `changepacks-node` dev-dependency, which is unavailable on the
//! feature path.

use std::path::Path;

#[cfg(test)]
use changepacks_core::{Package, Project};
#[cfg(test)]
use changepacks_node::package::NodePackage;
#[cfg(test)]
use std::path::PathBuf;

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
