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

use std::path::Path;

use anyhow::{Context, Result};
use changepacks_utils::write_finalized;
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
/// [`write_finalized`] to preserve formatting, comments, and the complete
/// trailing-whitespace suffix.
///
/// # Errors
/// Returns error if the file cannot be read or the TOML cannot be parsed.
pub(crate) async fn read_and_parse_cargo_toml(path: &Path) -> Result<(String, DocumentMut)> {
    let cargo_toml_raw = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read Cargo.toml {}", path.display()))?;
    let cargo_toml: DocumentMut = cargo_toml_raw
        .parse::<DocumentMut>()
        .with_context(|| format!("Failed to parse Cargo.toml {}", path.display()))?;
    Ok((cargo_toml_raw, cargo_toml))
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
/// Shared by future paths that need the same skeleton so a single edit
/// here keeps the format-preservation invariants in one place — matching
/// the Node/Python/Dart/CSharp convention documented in
/// `crates/AGENTS.md`.
///
/// An empty `[package]` table is created if missing. The explicit
/// `Table::new()` matters: plain `doc["package"]["version"] = ...`
/// auto-creates an INLINE table (`package = { version = ... }`) at the top
/// of the document instead of a proper `[package]` header — the same hazard
/// guarded in `changepacks-python`'s `write_pyproject_version`.
///
/// # Errors
/// Returns error if the file cannot be read, the TOML cannot be parsed,
/// or the write fails.
pub(crate) async fn write_cargo_package_version(path: &Path, new_version: &str) -> Result<()> {
    let (cargo_toml_raw, mut cargo_toml) = read_and_parse_cargo_toml(path).await?;
    if cargo_toml
        .get("package")
        .is_some_and(|package| !package.is_table_like())
    {
        anyhow::bail!(
            "Cargo.toml {} has a non-table [package] item",
            path.display()
        );
    }
    if cargo_toml.get("package").is_none() {
        cargo_toml["package"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    cargo_toml["package"]["version"] = new_version.into();
    write_finalized(path, cargo_toml.to_string(), &cargo_toml_raw, "Cargo.toml").await
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
        .and_then(|w| w.as_bool())
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
}
