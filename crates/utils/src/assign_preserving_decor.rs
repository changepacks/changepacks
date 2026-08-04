use toml_edit::Item;

/// Overwrite `slot` with the string `new_value`, carrying the previous value's
/// [`toml_edit::Decor`] across the assignment.
///
/// Every TOML manifest version rewrite replaces a whole [`toml_edit::Item`]
/// with a freshly built one, and a fresh value carries DEFAULT (empty) decor.
/// Decor is where `toml_edit` keeps the trivia around a value — the spacing
/// after `=` and, most visibly, an end-of-line comment such as
/// `version = "1.2.3" # pinned by release tooling`. Without this capture and
/// restore, a routine version bump silently deletes that comment from the
/// user's manifest, which is exactly the format-preservation guarantee this
/// tool advertises.
///
/// `changepacks-rust` factored this policy out for its SIX `Cargo.toml` write
/// sites, while `changepacks-python`'s `write_pyproject_version` open-coded the
/// identical capture / assign / restore triple inline. `crates/AGENTS.md`
/// forbids importing one language crate into another, so `changepacks-utils`
/// is the only legal home for the shared body — the same precedent already set
/// by [`crate::ensure_toml_table_like`]. Both crates now call this function
/// directly — no per-crate pass-through wrapper.
///
/// A slot that does not currently hold a value — a missing key auto-vivified
/// by `toml_edit` indexing, or a table — has no decor to preserve, so the
/// restore is skipped and behaviour is identical to a plain assignment.
///
/// Lives behind the crate's optional `toml` feature so `changepacks-utils`
/// gains no unconditional `toml_edit` dependency: only `changepacks-rust` and
/// `changepacks-python` enable it, and both already depended on `toml_edit`
/// directly, leaving the binary's dependency closure unchanged.
pub fn assign_preserving_decor(slot: &mut Item, new_value: &str) {
    // Cloned BEFORE the assignment: `*slot = ...` drops the old value.
    let previous_decor = slot.as_value().map(|value| value.decor().clone());
    *slot = toml_edit::value(new_value);
    if let (Some(decor), Some(value)) = (previous_decor, slot.as_value_mut()) {
        *value.decor_mut() = decor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::DocumentMut;

    fn doc(raw: &str) -> DocumentMut {
        raw.parse::<DocumentMut>().unwrap()
    }

    /// The headline case: an end-of-line comment on the version line is stored
    /// as the VALUE's suffix decor and must survive the rewrite.
    #[test]
    fn test_assign_preserving_decor_keeps_trailing_comment() {
        let mut manifest = doc("[package]\nversion = \"1.0.0\" # pinned\n");

        assign_preserving_decor(&mut manifest["package"]["version"], "2.0.0");

        assert_eq!(
            manifest.to_string(),
            "[package]\nversion = \"2.0.0\" # pinned\n"
        );
    }

    /// A slot that holds no value — here a key auto-vivified by `toml_edit`
    /// indexing into a table that does not declare it — has no decor to carry,
    /// so the restore must be skipped rather than panicking, and the result is
    /// a plain assignment with default spacing.
    #[test]
    fn test_assign_preserving_decor_handles_slot_without_value() {
        let mut manifest = doc("[package]\nname = \"x\"\n");
        // `IndexMut` on a missing key auto-vivifies a NON-value `Item`, so
        // `as_value()` is `None` and nothing is captured.
        assert!(
            manifest["package"].get("version").is_none(),
            "the fixture must not declare a version yet"
        );

        assign_preserving_decor(&mut manifest["package"]["version"], "0.0.1");

        assert_eq!(
            manifest.to_string(),
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\n"
        );
    }

    /// Replacing a table-valued slot is likewise decor-free: `as_value()`
    /// returns `None` for a table, so the assignment is unconditional.
    #[test]
    fn test_assign_preserving_decor_replaces_table_slot() {
        let mut manifest = doc("[package]\n[package.version]\nworkspace = true\n");

        assign_preserving_decor(&mut manifest["package"]["version"], "1.2.3");

        assert_eq!(
            manifest["package"]["version"].as_str(),
            Some("1.2.3"),
            "a table slot must be replaced by the plain string value"
        );
    }

    /// Non-comment trivia counts too: unusual spacing around `=` lives in the
    /// value's prefix decor and must not be normalized away by a bump.
    #[test]
    fn test_assign_preserving_decor_keeps_unusual_spacing() {
        let mut manifest = doc("[package]\nversion =    \"1.0.0\"\n");

        assign_preserving_decor(&mut manifest["package"]["version"], "1.0.1");

        assert_eq!(manifest.to_string(), "[package]\nversion =    \"1.0.1\"\n");
    }
}
