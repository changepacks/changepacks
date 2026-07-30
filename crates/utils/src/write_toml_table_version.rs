use std::path::Path;

use anyhow::Result;
use toml_edit::DocumentMut;

use crate::{assign_preserving_decor, ensure_toml_table_like, read_and_parse, write_finalized};

/// Set `[<table_key>].version` of the TOML manifest at `path` to
/// `new_version`, preserving the file's formatting, comments and complete
/// trailing-whitespace shape.
///
/// `changepacks-rust`'s `write_cargo_package_version` (`[package]` in
/// `Cargo.toml`) and `changepacks-python`'s `write_pyproject_version`
/// (`[project]` in `pyproject.toml`) open-coded the SAME five-step skeleton:
///
/// 1. [`read_and_parse`] the manifest with `str::parse::<DocumentMut>`,
///    keeping the raw text for the write-back,
/// 2. [`ensure_toml_table_like`] to reject a present-but-scalar top-level key
///    BEFORE any mutation,
/// 3. create the top-level table when the key is absent,
/// 4. [`assign_preserving_decor`] onto the `version` slot,
/// 5. [`write_finalized`] to replay the original trailing whitespace.
///
/// They differed only in the manifest `label`, the `table_key`, and one
/// Python-only `project.dynamic` guard. `crates/AGENTS.md` forbids importing
/// one language crate into another, so `changepacks-utils` is the only legal
/// home for the shared body — the same precedent already set by
/// [`ensure_toml_table_like`] and [`assign_preserving_decor`].
///
/// `validate` carries the per-language extra rule. It runs AFTER the
/// table-like guard (so a malformed manifest is still rejected by the
/// strictest, cheapest check first) and BEFORE the table is created (so a
/// rejected manifest is never mutated and never written). Callers with no
/// extra rule pass a closure that returns `Ok(())`.
///
/// Creating the missing table explicitly via [`toml_edit::Table::new`] is not
/// cosmetic: plain `doc[key]["version"] = ...` auto-creates an INLINE table
/// (`package = { version = ... }`) at the top of the document instead of a
/// proper `[package]` / `[project]` header.
///
/// Lives behind the crate's optional `toml` feature so `changepacks-utils`
/// gains no unconditional `toml_edit` dependency: only `changepacks-rust` and
/// `changepacks-python` enable it, and both already depended on `toml_edit`
/// directly, leaving the binary's dependency closure unchanged.
///
/// # Errors
/// Returns an error when the file cannot be read, the TOML cannot be parsed,
/// `table_key` is present but not table-like, `validate` rejects the parsed
/// document, or the write-back fails. Every error names `label` and `path`.
pub async fn write_toml_table_version(
    path: &Path,
    label: &str,
    table_key: &str,
    new_version: &str,
    validate: impl FnOnce(&DocumentMut) -> Result<()>,
) -> Result<()> {
    let (raw, mut doc) = read_and_parse(path, label, str::parse::<DocumentMut>).await?;
    let has_table = ensure_toml_table_like(&doc, path, table_key, label)?;
    validate(&doc)?;
    if !has_table {
        doc[table_key] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    // The indexed slot IS the value whose decor is preserved: when
    // `[table_key].version` exists it is that value, and when the table was
    // just created — or exists without a `version` — `toml_edit` auto-vivifies
    // a non-value `Item`, so there is no decor to carry and the helper falls
    // back to a plain assignment.
    assign_preserving_decor(&mut doc[table_key]["version"], new_version);
    write_finalized(path, doc.to_string(), &raw, label).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn ok(_doc: &DocumentMut) -> Result<()> {
        Ok(())
    }

    /// The happy path: only the version literal changes, and the surrounding
    /// comment/decor and the exact trailing-whitespace suffix survive.
    #[tokio::test]
    async fn test_write_toml_table_version_rewrites_only_the_version() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("Cargo.toml");
        let original = "# head\n[package]\nname = \"x\"\nversion = \"1.0.0\" # pinned\n \t\r\n \n";
        fs::write(&manifest, original).unwrap();

        write_toml_table_version(&manifest, "Cargo.toml", "package", "2.0.0", ok)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&manifest).unwrap(),
            original.replace("1.0.0", "2.0.0")
        );
    }

    /// A missing top-level table is materialized as a proper header, never as
    /// the inline-table form `package = { version = ... }`.
    #[tokio::test]
    async fn test_write_toml_table_version_creates_proper_header() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("pyproject.toml");
        fs::write(&manifest, "[build-system]\nrequires = []\n").unwrap();

        write_toml_table_version(&manifest, "pyproject.toml", "project", "0.0.1", ok)
            .await
            .unwrap();

        let written = fs::read_to_string(&manifest).unwrap();
        assert!(
            written.lines().any(|line| line.trim() == "[project]"),
            "output must contain a literal [project] header line, got: {written}"
        );
        assert!(
            !written.contains("project = {"),
            "output must not use the inline-table form, got: {written}"
        );
    }

    /// The table-like guard runs before anything is written, so a scalar
    /// top-level key leaves the manifest byte-identical.
    #[tokio::test]
    async fn test_write_toml_table_version_non_table_key_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("Cargo.toml");
        let original = "package = 3\n\n[dependencies]\nserde = \"1\"\n";
        fs::write(&manifest, original).unwrap();

        let err = write_toml_table_version(&manifest, "Cargo.toml", "package", "1.0.1", ok)
            .await
            .expect_err("a scalar top-level key must be rejected");

        assert!(
            format!("{err:#}").contains("has a non-table [package] item"),
            "error chain should name the table-like guard, got: {err:#}"
        );
        assert_eq!(fs::read(&manifest).unwrap(), original.as_bytes());
    }

    /// A rejecting `validate` aborts before the table creation and before the
    /// write, so the manifest is never touched.
    #[tokio::test]
    async fn test_write_toml_table_version_validate_rejection_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("pyproject.toml");
        let original = "[build-system]\nrequires = []\n";
        fs::write(&manifest, original).unwrap();

        let err = write_toml_table_version(&manifest, "pyproject.toml", "project", "1.0.1", |_| {
            anyhow::bail!("validator says no")
        })
        .await
        .expect_err("a rejecting validator must fail the write");

        assert!(
            format!("{err:#}").contains("validator says no"),
            "error chain should carry the validator's message, got: {err:#}"
        );
        assert_eq!(
            fs::read(&manifest).unwrap(),
            original.as_bytes(),
            "a rejected bump must leave the manifest byte-identical"
        );
    }

    /// Guard ordering is load-bearing: when BOTH the table-like guard and
    /// `validate` would reject, the table-like guard must win, so the shared
    /// helper cannot silently change either crate's user-visible message.
    #[tokio::test]
    async fn test_write_toml_table_version_table_guard_precedes_validate() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("pyproject.toml");
        fs::write(&manifest, "project = 3\n").unwrap();

        let err = write_toml_table_version(&manifest, "pyproject.toml", "project", "1.0.1", |_| {
            anyhow::bail!("validator says no")
        })
        .await
        .expect_err("a scalar top-level key must be rejected");

        let chain = format!("{err:#}");
        assert!(
            chain.contains("has a non-table [project] item"),
            "the table-like guard must report first, got: {chain}"
        );
        assert!(
            !chain.contains("validator says no"),
            "validate must not run after the table-like guard failed, got: {chain}"
        );
    }

    /// A malformed manifest fails in [`read_and_parse`], carrying the caller's
    /// label and the path.
    #[tokio::test]
    async fn test_write_toml_table_version_parse_error_names_label_and_path() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("Cargo.toml");
        fs::write(&manifest, "[package\n").unwrap();

        let err = write_toml_table_version(&manifest, "Cargo.toml", "package", "1.0.1", ok)
            .await
            .expect_err("a malformed manifest must fail the parse");

        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to parse Cargo.toml {}",
                manifest.display()
            )),
            "error chain should carry the parse label and path context, got: {chain}"
        );
    }
}
