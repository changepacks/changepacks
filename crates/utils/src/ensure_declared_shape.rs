use std::path::Path;

use anyhow::Result;

/// Validate an OPTIONAL manifest declaration whose mere presence must carry a
/// specific shape, and report whether it was declared at all.
///
/// The Node, Dart and Python finders each opened the same seven-line `match`
/// over their workspace declaration before calling [`crate::is_workspace_by_sibling`]:
/// an absent field means "not declared" (`false`), a well-shaped field means
/// "declared" (`true`), and a wrongly-shaped field aborts the walk with the
/// byte-identical template `Invalid `<field>` declaration in <path>: expected
/// <expected>`. Only the field name, the expectation wording and the shape
/// predicate differed, and every caller already computed that predicate against
/// its own manifest value type.
///
/// `declaration` therefore carries the caller's *already-evaluated* answer:
/// - `None` — the field is absent (`Ok(false)`),
/// - `Some(true)` — the field is present and well-shaped (`Ok(true)`),
/// - `Some(false)` — the field is present with the wrong shape (`Err`).
///
/// Passing the collapsed `Option<bool>` rather than the manifest value is what
/// keeps this helper dependency-free: `changepacks-utils` never sees
/// `serde_json::Value`, `yaml_serde::Value` or `toml_edit::Item`, so no crate
/// gains a dependency and no optional feature is needed. `crates/AGENTS.md`
/// forbids importing one language crate into another, so `changepacks-utils`
/// is the only legal home for the shared body.
///
/// Callers build `declaration` with `Option::map` over their own lookup, e.g.
/// `package_json.get("workspaces").map(|w| w.is_array() || w.is_object())`.
///
/// # Errors
/// Returns an error naming `manifest_path` when `declaration` is `Some(false)`,
/// i.e. the field is present but does not have the expected shape.
pub fn ensure_declared_shape(
    declaration: Option<bool>,
    manifest_path: &Path,
    field: &str,
    expected: &str,
) -> Result<bool> {
    match declaration {
        None => Ok(false),
        Some(true) => Ok(true),
        Some(false) => anyhow::bail!(
            "Invalid `{field}` declaration in {}: expected {expected}",
            manifest_path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absent field is NOT an error — the caller falls back to its sibling
    /// marker file (`pnpm-workspace.yaml`, `melos.yaml`) or to "plain package".
    #[test]
    fn test_absent_declaration_is_not_declared() {
        assert!(
            !ensure_declared_shape(
                None,
                Path::new("package.json"),
                "workspaces",
                "an array or object",
            )
            .unwrap()
        );
    }

    /// A well-shaped field short-circuits the caller straight to "workspace".
    #[test]
    fn test_valid_declaration_is_declared() {
        assert!(
            ensure_declared_shape(
                Some(true),
                Path::new("package.json"),
                "workspaces",
                "an array or object",
            )
            .unwrap()
        );
    }

    /// The Node rejection message must stay byte-identical to the one the
    /// finder published before the extraction.
    #[test]
    fn test_invalid_declaration_message_matches_node_template() {
        let manifest = Path::new("some").join("package.json");
        let err = ensure_declared_shape(Some(false), &manifest, "workspaces", "an array or object")
            .expect_err("a wrongly-shaped `workspaces` field must be rejected");

        assert_eq!(
            format!("{err}"),
            format!(
                "Invalid `workspaces` declaration in {}: expected an array or object",
                manifest.display()
            )
        );
    }

    /// The Dart field/expectation pair reuses the same body unchanged.
    #[test]
    fn test_invalid_declaration_message_matches_dart_template() {
        let manifest = Path::new("some").join("pubspec.yaml");
        let err = ensure_declared_shape(Some(false), &manifest, "workspace", "a sequence")
            .expect_err("a wrongly-shaped `workspace` field must be rejected");

        assert_eq!(
            format!("{err}"),
            format!(
                "Invalid `workspace` declaration in {}: expected a sequence",
                manifest.display()
            )
        );
    }

    /// The Python field name is a dotted TOML path, and it is interpolated
    /// verbatim inside the backticks — no escaping or re-quoting.
    #[test]
    fn test_invalid_declaration_message_matches_python_template() {
        let manifest = Path::new("some").join("pyproject.toml");
        let err = ensure_declared_shape(
            Some(false),
            &manifest,
            "[tool.uv].workspace",
            "a table or inline table",
        )
        .expect_err("a wrongly-shaped `[tool.uv].workspace` field must be rejected");

        assert_eq!(
            format!("{err}"),
            format!(
                "Invalid `[tool.uv].workspace` declaration in {}: expected a table or inline table",
                manifest.display()
            )
        );
    }
}
