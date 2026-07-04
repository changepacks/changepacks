//! # changepacks-python
//!
//! Python project support for changepacks.
//!
//! Implements project discovery and version management for pyproject.toml files. Parses
//! TOML using the toml crate and preserves formatting when updating versions. Supports
//! both single packages and workspace configurations.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::PythonProjectFinder;

/// Default publish command for Python projects. Shared by `PythonPackage`
/// and `PythonWorkspace` so a single edit here updates both trait impls.
pub(crate) const PUBLISH_COMMAND: &str = "uv publish";

/// Default dry-run publish command for Python projects.
/// `uv publish --dry-run` is `uv`'s built-in non-mutating verification;
/// users can override via `publishDryRun` in `.changepacks/config.json`.
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = "uv publish --dry-run";

use std::path::Path;

use anyhow::Result;
use changepacks_core::UpdateType;
use changepacks_utils::{finalize_content, next_version_or_default};
use tokio::fs::{read_to_string, write};
use toml_edit::DocumentMut;

/// Shared body for `PythonPackage::update_version` and
/// `PythonWorkspace::update_version`.
///
/// Consolidates the "read current version, compute next, write pyproject,
/// stash new version on `self`" 5-line sequence — previously duplicated
/// between `PythonPackage` and `PythonWorkspace` differing ONLY in the
/// `ensure_project_table` bool passed to `write_pyproject_version` — into
/// ONE source of truth. Both trait impls now delegate here so a future
/// rewording of the "reserve `0.0.0` when unversioned" fallback lands in
/// exactly one place. The `ensure_project_table` parameter keeps the
/// workspace vs. package behaviour distinct at the call site
/// (`false` for `PythonPackage`, `true` for `PythonWorkspace` — matching
/// the pre-consolidation bodies exactly).
///
/// A shared helper (rather than a parameterized `macro_rules!` producing
/// `async fn`) is required because `#[async_trait]` runs BEFORE
/// declarative-macro expansion — see the twin helper in
/// `crates/dart/src/lib.rs` for the full E0195 rationale.
///
/// # Errors
/// Returns error if the version update or pyproject write fails.
pub(crate) async fn update_version_from_fields(
    version: &mut Option<String>,
    path: &Path,
    update_type: UpdateType,
    ensure_project_table: bool,
) -> Result<()> {
    // Two-line "reserve `0.0.0` when unversioned" prelude consolidated
    // into `changepacks_utils::next_version_or_default` so the fallback
    // policy lives in ONE place across every language crate. See that
    // helper's doc for the Java/Rust carve-outs.
    let new_version = next_version_or_default(version.as_deref(), update_type)?;

    write_pyproject_version(path, &new_version, ensure_project_table).await?;
    *version = Some(new_version);
    Ok(())
}

/// Update `pyproject.toml` at `path` to set `[project].version` to
/// `new_version`, preserving the file's trailing-newline shape (via
/// `trailing_newline`) and its TOML formatting (via `toml_edit`).
///
/// Shared by `PythonPackage::update_version` and
/// `PythonWorkspace::update_version` so both paths emit byte-identical output.
/// When `ensure_project_table` is `true`, an empty `[project]` table is
/// created if missing (needed for workspace roots that only declare
/// `[tool.uv.workspace]`).
///
/// # Errors
/// Returns error if the file cannot be read, is not valid TOML, or the write
/// fails.
pub(crate) async fn write_pyproject_version(
    path: &Path,
    new_version: &str,
    ensure_project_table: bool,
) -> Result<()> {
    let pyproject_toml_raw = read_to_string(path).await?;
    let mut pyproject_toml: DocumentMut = pyproject_toml_raw.parse::<DocumentMut>()?;
    if ensure_project_table && pyproject_toml.get("project").is_none() {
        pyproject_toml["project"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    pyproject_toml["project"]["version"] = new_version.into();
    write(
        path,
        finalize_content(&pyproject_toml.to_string(), &pyproject_toml_raw),
    )
    .await?;
    Ok(())
}
