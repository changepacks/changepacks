use toml_edit::Item;

/// Read `<item>.<field>` out of an optional TOML table-like item as an owned
/// `String`, yielding `None` when the item is absent, when the field is
/// missing, or when the field holds anything other than a plain string.
///
/// `changepacks-rust`'s `package_str` (`[package].<field>` in `Cargo.toml`),
/// its `workspace_package_str` sibling (`[workspace.package].<field>`) and
/// `changepacks-python`'s `project_str` (`[project].<field>` in
/// `pyproject.toml`) were byte-for-byte the same
/// `and_then(get field) → and_then(as_str) → map(to owned)` chain, differing
/// only in how each located the table it started from. `crates/AGENTS.md`
/// forbids importing one language crate into another, so `changepacks-utils`
/// is the only legal home for the shared tail; the two Rust helpers keep
/// their names and signatures and now delegate here, so no call site outside
/// their finders changed.
///
/// Taking `Option<&Item>` rather than a `&DocumentMut` plus a table key is
/// what lets all three callers share it: each already had to locate its own
/// table (`doc.get("package")`, `workspace_package_table(doc)`,
/// `pyproject_toml.get("project")`), and those lookups are not interchangeable.
///
/// Only string-valued fields resolve. An inheritance marker such as
/// `version = { workspace = true }` or a numeric value is table-like /
/// non-string, so it deliberately returns `None` and lets the caller fall back
/// to its workspace-inherited answer, exactly as the code it replaces did.
///
/// Lives behind the crate's optional `toml` feature so `changepacks-utils`
/// gains no unconditional `toml_edit` dependency: only `changepacks-rust` and
/// `changepacks-python` enable it, and both already depended on `toml_edit`
/// directly, leaving the binary's dependency closure unchanged.
#[must_use]
pub fn toml_item_str(item: Option<&Item>, field: &str) -> Option<String> {
    item.and_then(|i| i.get(field))
        .and_then(|v| v.as_str())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::DocumentMut;

    fn doc(raw: &str) -> DocumentMut {
        raw.parse::<DocumentMut>().unwrap()
    }

    /// An absent table (the `pyproject.toml` without a `[project]` header, or
    /// a `Cargo.toml` virtual workspace root without `[package]`) resolves to
    /// `None` instead of panicking.
    #[test]
    fn test_toml_item_str_none_item() {
        assert_eq!(toml_item_str(None, "version"), None);
    }

    /// A present table missing the requested field resolves to `None`.
    #[test]
    fn test_toml_item_str_missing_field() {
        let document = doc("[package]\nname = \"demo\"\n");
        assert_eq!(toml_item_str(document.get("package"), "version"), None);
    }

    /// A non-string value must NOT be coerced: `version.workspace = true` is
    /// the Cargo inheritance marker, and returning `None` is what makes the
    /// caller fall back to `[workspace.package].version`.
    #[test]
    fn test_toml_item_str_non_string_value() {
        let document = doc("[package]\nversion = { workspace = true }\nedition = 2024\n");
        assert_eq!(toml_item_str(document.get("package"), "version"), None);
        assert_eq!(toml_item_str(document.get("package"), "edition"), None);
    }

    /// The plain-string hit returns the value without its quotes.
    #[test]
    fn test_toml_item_str_plain_string_hit() {
        let document = doc("[project]\nname = \"demo\"\nversion = \"1.2.3\"\n");
        assert_eq!(
            toml_item_str(document.get("project"), "version"),
            Some("1.2.3".to_string())
        );
        assert_eq!(
            toml_item_str(document.get("project"), "name"),
            Some("demo".to_string())
        );
    }

    /// A nested table reached through an intermediate lookup works the same
    /// way — this is the `[workspace.package]` shape the Rust finder uses.
    #[test]
    fn test_toml_item_str_reads_nested_table() {
        let document = doc("[workspace.package]\nversion = \"0.9.0\"\n");
        let workspace_package = document.get("workspace").and_then(|w| w.get("package"));
        assert_eq!(
            toml_item_str(workspace_package, "version"),
            Some("0.9.0".to_string())
        );
    }

    /// A scalar in the table position has no `field` to offer, so the lookup
    /// resolves to `None` rather than erroring.
    #[test]
    fn test_toml_item_str_scalar_table_position() {
        let document = doc("package = 3\n");
        assert_eq!(toml_item_str(document.get("package"), "version"), None);
    }
}
