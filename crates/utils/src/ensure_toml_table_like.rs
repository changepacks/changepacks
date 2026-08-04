use std::path::Path;

use anyhow::Result;
use toml_edit::DocumentMut;

/// Reject a TOML manifest whose top-level `key` exists but is NOT table-like
/// (e.g. `package = 3` or `project = "not-a-table"`), and report whether the
/// key is present at all.
///
/// `changepacks-rust`'s `ensure_package_table_like` (`[package]` in
/// `Cargo.toml`) and `changepacks-python`'s `ensure_project_table_like`
/// (`[project]` in `pyproject.toml`) were byte-for-byte the same function
/// modulo the key name and the manifest label — the Python doc comment even
/// described itself as "the mirror of `changepacks_rust`'s
/// `ensure_package_table_like`". `crates/AGENTS.md` forbids importing one
/// language crate into another, so `changepacks-utils` is the only legal home
/// for the shared body; both language helpers keep their names and
/// `pub(crate)` visibility and now delegate here, so no call site outside
/// their `lib.rs` changed.
///
/// `label` is the human-facing manifest name and `key` the top-level table
/// name, together forming the unchanged user-visible message
/// `<label> <path> has a non-table [<key>] item`.
///
/// Callers reuse the returned flag for their own control flow — creating the
/// missing table in the package/project writers, or driving the
/// hybrid/virtual-root branch in the Rust workspace writer — so the shared
/// helper performs exactly ONE document lookup, same as the code it replaces.
///
/// Guarding BEFORE any mutation is the point: `toml_edit` indexing assignment
/// would otherwise silently replace the scalar and rewrite the manifest.
///
/// Lives behind the crate's optional `toml` feature so `changepacks-utils`
/// gains no unconditional `toml_edit` dependency: only `changepacks-rust` and
/// `changepacks-python` enable it, and both already depended on `toml_edit`
/// directly, leaving the binary's dependency closure unchanged.
///
/// # Errors
/// Returns an error naming `path` when `key` is present but not table-like.
pub fn ensure_toml_table_like(
    doc: &DocumentMut,
    path: &Path,
    key: &str,
    label: &str,
) -> Result<bool> {
    let item = doc.get(key);
    if item.is_some_and(|item| !item.is_table_like()) {
        anyhow::bail!("{label} {} has a non-table [{key}] item", path.display());
    }
    Ok(item.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(raw: &str) -> DocumentMut {
        raw.parse::<DocumentMut>().unwrap()
    }

    /// A standard `[package]` header is table-like and present.
    #[test]
    fn test_ensure_toml_table_like_reports_present_table() {
        let manifest = Path::new("Cargo.toml");
        assert!(
            ensure_toml_table_like(
                &doc("[package]\nversion = \"1.0.0\"\n"),
                manifest,
                "package",
                "Cargo.toml",
            )
            .unwrap()
        );
    }

    /// An INLINE table (`project = { ... }`) is still table-like, so it must be
    /// accepted rather than rejected as a scalar.
    #[test]
    fn test_ensure_toml_table_like_accepts_inline_table() {
        let manifest = Path::new("pyproject.toml");
        assert!(
            ensure_toml_table_like(
                &doc("project = { version = \"1.0.0\" }\n"),
                manifest,
                "project",
                "pyproject.toml",
            )
            .unwrap()
        );
    }

    /// A missing key is NOT an error — the caller creates the table itself.
    #[test]
    fn test_ensure_toml_table_like_reports_missing_key() {
        let manifest = Path::new("pyproject.toml");
        assert!(
            !ensure_toml_table_like(
                &doc("[build-system]\nrequires = []\n"),
                manifest,
                "project",
                "pyproject.toml",
            )
            .unwrap()
        );
    }

    /// The rejection message must interpolate label, path and key in the exact
    /// shape both language crates published before the extraction.
    #[test]
    fn test_ensure_toml_table_like_error_names_label_path_and_key() {
        let manifest = Path::new("some").join("Cargo.toml");
        let err = ensure_toml_table_like(&doc("package = 3\n"), &manifest, "package", "Cargo.toml")
            .expect_err("a scalar `package` must be rejected");

        assert_eq!(
            format!("{err}"),
            format!(
                "Cargo.toml {} has a non-table [package] item",
                manifest.display()
            )
        );
    }

    /// The same body serves the Python key/label pair unchanged.
    #[test]
    fn test_ensure_toml_table_like_error_uses_caller_key_and_label() {
        let manifest = Path::new("pyproject.toml");
        let err = ensure_toml_table_like(
            &doc("project = \"not-a-table\"\n"),
            manifest,
            "project",
            "pyproject.toml",
        )
        .expect_err("a scalar `project` must be rejected");

        assert_eq!(
            format!("{err}"),
            format!(
                "pyproject.toml {} has a non-table [project] item",
                manifest.display()
            )
        );
    }

    /// An ARRAY value (`package = [...]`) is a non-scalar that is still not
    /// table-like, so it must take the reject path instead of being indexed
    /// into and silently overwritten by the version writer.
    #[test]
    fn test_ensure_toml_table_like_rejects_array_value() {
        let manifest = Path::new("some").join("Cargo.toml");
        let err = ensure_toml_table_like(
            &doc("package = [\"a\", \"b\"]\n"),
            &manifest,
            "package",
            "Cargo.toml",
        )
        .expect_err("an array `package` must be rejected");

        assert_eq!(
            format!("{err}"),
            format!(
                "Cargo.toml {} has a non-table [package] item",
                manifest.display()
            )
        );
    }

    /// An ARRAY OF TABLES (a doubled `[[package]]` header) parses to
    /// `Item::ArrayOfTables`, which `toml_edit` does NOT report as table-like
    /// even though each element is a table — so this typo is rejected too.
    #[test]
    fn test_ensure_toml_table_like_rejects_array_of_tables() {
        let manifest = Path::new("some").join("pyproject.toml");
        let err = ensure_toml_table_like(
            &doc("[[project]]\nversion = \"1.0.0\"\n"),
            &manifest,
            "project",
            "pyproject.toml",
        )
        .expect_err("an array-of-tables `project` must be rejected");

        assert_eq!(
            format!("{err}"),
            format!(
                "pyproject.toml {} has a non-table [project] item",
                manifest.display()
            )
        );
    }
}
