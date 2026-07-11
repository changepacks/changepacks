//! Shared `#[cfg(test)]` fixtures for this crate's unit tests.
//!
//! Declared `#[cfg(test)]` in `lib.rs`, so nothing here is compiled into a
//! non-test build. Relies on the `changepacks-node` dev-dependency that every
//! consuming test module already imports.

use std::path::{Path, PathBuf};

use changepacks_core::{Package, Project};
use changepacks_node::package::NodePackage;

/// Create a test project (a `NodePackage` at version `1.0.0`) with the given
/// dependencies.
///
/// Dependencies are stored as *names* (e.g. `"p2"`), matching every real
/// finder — `add_dependency` records the raw name, never a relative path — so
/// `sort_by_dependencies` resolves them by name via `name_to_index` and
/// `apply_reverse_dependencies` resolves them via `reverse_deps`.
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
pub(crate) fn run_git(path: &Path, args: &[&str]) {
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
pub(crate) fn init_git_repo(path: &Path) {
    run_git(path, &["init", "-b", "main"]);
    run_git(path, &["config", "user.email", "test@test.com"]);
    run_git(path, &["config", "user.name", "Test"]);
}

/// Stage all changes and commit them with `message` in the repo at `path`.
pub(crate) fn git_add_and_commit(path: &Path, message: &str) {
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", message]);
}
