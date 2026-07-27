//! # changepacks-python
//!
//! Python project support for changepacks.
//!
//! Implements project discovery and version management for pyproject.toml files. Parses
//! TOML using `toml_edit` for non-destructive formatting preservation when updating
//! versions. Supports both single packages and workspace configurations.

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
use changepacks_utils::{read_and_parse, write_finalized};
use toml_edit::DocumentMut;

/// Read and parse a pyproject.toml file, returning both the raw content
/// (for trailing-newline preservation) and the parsed TOML document.
///
/// The read-then-parse-with-context sequence lives in
/// [`changepacks_utils::read_and_parse`] — the mirror of [`write_finalized`] —
/// so only the `pyproject.toml` label and the `toml_edit` parser stay here.
///
/// # Errors
/// Returns error if the file cannot be read or is not valid TOML.
pub(crate) async fn read_and_parse_pyproject_toml(path: &Path) -> Result<(String, DocumentMut)> {
    read_and_parse(path, "pyproject.toml", str::parse::<DocumentMut>).await
}

/// Update `pyproject.toml` at `path` to set `[project].version` to
/// `new_version`, preserving the file's complete trailing-whitespace shape
/// (via `write_finalized`) and its TOML formatting (via `toml_edit`).
///
/// Shared by `PythonPackage::update_version` and
/// `PythonWorkspace::update_version` so both paths emit byte-identical output.
/// An empty `[project]` table is created if missing — needed for workspace
/// roots that only declare `[tool.uv.workspace]` and for `[build-system]`-only
/// package manifests (a valid PEP 517 shape). The explicit `Table::new()`
/// matters: plain `doc["project"]["version"] = ...` auto-creates an INLINE
/// table (`project = { version = ... }`) at the top of the document instead
/// of a proper `[project]` header.
///
/// # Errors
/// Returns error if the file cannot be read, is not valid TOML, or the write
/// fails.
pub(crate) async fn write_pyproject_version(path: &Path, new_version: &str) -> Result<()> {
    let (pyproject_toml_raw, mut pyproject_toml) = read_and_parse_pyproject_toml(path).await?;
    if pyproject_toml
        .get("project")
        .is_some_and(|project| !project.is_table_like())
    {
        anyhow::bail!(
            "pyproject.toml {} has a non-table [project] item",
            path.display()
        );
    }
    let has_dynamic_version = pyproject_toml
        .get("project")
        .and_then(|project| project.get("dynamic"))
        .and_then(toml_edit::Item::as_array)
        .is_some_and(|dynamic| dynamic.iter().any(|item| item.as_str() == Some("version")));
    if has_dynamic_version {
        anyhow::bail!(
            "pyproject.toml {} has backend-managed version in project.dynamic",
            path.display()
        );
    }
    if pyproject_toml.get("project").is_none() {
        pyproject_toml["project"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    pyproject_toml["project"]["version"] = new_version.into();
    write_finalized(
        path,
        pyproject_toml.to_string(),
        &pyproject_toml_raw,
        "pyproject.toml",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_pyproject_version_preserves_complete_trailing_whitespace() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        let suffix = " \t\r\n \n";
        fs::write(
            &pyproject_toml,
            format!("[project]\nversion = \"1.0.0\"{suffix}"),
        )
        .unwrap();

        write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&pyproject_toml).unwrap(),
            format!("[project]\nversion = \"2.0.0\"{suffix}")
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(&pyproject_toml, "[project]\nversion = \"1.0.0\"\n").unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        test_support::set_readonly(&pyproject_toml, true);

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result = write_pyproject_version(&pyproject_toml, "2.0.0").await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        test_support::set_readonly(&pyproject_toml, false);

        let err = result.expect_err("write to a readonly pyproject.toml must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_non_table_project_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(&pyproject_toml, "project = 3\n").unwrap();

        let err = write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .expect_err("non-table project item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("non-table [project]"),
            "error chain should mention the non-table project item, got: {chain}"
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_rejects_dynamic_version_multiline() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        let content = "[project]\ndynamic = [ \"version\" ]\n";
        fs::write(&pyproject_toml, content).unwrap();

        let err = write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .expect_err("dynamic version must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("project.dynamic"),
            "error chain should mention project.dynamic, got: {chain}"
        );

        let after = fs::read(&pyproject_toml).unwrap();
        assert_eq!(
            after,
            content.as_bytes(),
            "file bytes must be unchanged after rejection"
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_rejects_dynamic_version_compact() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        let content = "[project]\ndynamic = [\"version\"]\n";
        fs::write(&pyproject_toml, content).unwrap();

        let err = write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .expect_err("dynamic version must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("project.dynamic"),
            "error chain should mention project.dynamic, got: {chain}"
        );

        let after = fs::read(&pyproject_toml).unwrap();
        assert_eq!(
            after,
            content.as_bytes(),
            "file bytes must be unchanged after rejection"
        );
    }
}
