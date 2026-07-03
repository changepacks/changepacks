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

use std::path::Path;

use anyhow::Result;
use changepacks_utils::trailing_newline;
use tokio::fs::{read_to_string, write};
use toml_edit::DocumentMut;

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
        format!(
            "{}{}",
            pyproject_toml.to_string().trim_end(),
            trailing_newline(&pyproject_toml_raw)
        ),
    )
    .await?;
    Ok(())
}
