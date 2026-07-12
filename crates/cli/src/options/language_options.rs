use changepacks_core::{Language, Project};
use clap::ValueEnum;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliLanguage {
    Python,
    Node,
    Rust,
    Dart,
    Java,
    CSharp,
}

impl From<CliLanguage> for Language {
    fn from(value: CliLanguage) -> Self {
        match value {
            CliLanguage::Python => Self::Python,
            CliLanguage::Node => Self::Node,
            CliLanguage::Rust => Self::Rust,
            CliLanguage::Dart => Self::Dart,
            CliLanguage::Java => Self::Java,
            CliLanguage::CSharp => Self::CSharp,
        }
    }
}

/// Return whether `lang` matches any CLI-selected language in `langs`.
///
/// The single "does this project's language match the `--language` filter"
/// predicate, shared by [`retain_by_language`] (which filters a `Vec<&Project>`)
/// and the `update_map.retain(...)` closure in `commands/update.rs` (which
/// filters a `HashMap` via a precomputed path→language map). A future language
/// alias (e.g. `--language kotlin` mapping to `Language::Java`) now lands in
/// exactly one place. Both call sites short-circuit on the first match,
/// iterating `langs` in insertion order.
#[must_use]
pub fn language_slice_contains(langs: &[CliLanguage], lang: Language) -> bool {
    langs.iter().any(|&l| Language::from(l) == lang)
}

/// Retain only projects whose language matches one of `langs`.
///
/// No-op when `langs` is empty (the CLI convention: an empty `--language`
/// flag list means "no language filter applied"). Extracted from the
/// identical filter block that was previously open-coded in `check`,
/// `publish`, and `changepacks` — one place to evolve the semantics
/// (e.g. accept a `Language::Java` alias for `--language kotlin`).
///
/// Delegates the per-project predicate to [`language_slice_contains`], the
/// same match rule the `update_map.retain(...)` closure in
/// `commands/update.rs` uses — so both filter shells share one definition of
/// "does this language match the `--language` selection". Behavior is
/// byte-identical: the predicate short-circuits on the first match and
/// iterates `langs` in insertion order.
pub fn retain_by_language(langs: &[CliLanguage], projects: &mut Vec<&Project>) {
    if langs.is_empty() {
        return;
    }
    projects.retain(|project| language_slice_contains(langs, project.language()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    use crate::test_support::MockPackage;

    #[rstest]
    #[case(CliLanguage::Python, Language::Python)]
    #[case(CliLanguage::Node, Language::Node)]
    #[case(CliLanguage::Rust, Language::Rust)]
    #[case(CliLanguage::Dart, Language::Dart)]
    #[case(CliLanguage::Java, Language::Java)]
    #[case(CliLanguage::CSharp, Language::CSharp)]
    fn test_cli_language_to_language(#[case] cli_lang: CliLanguage, #[case] expected: Language) {
        let result: Language = cli_lang.into();
        assert_eq!(result, expected);
    }

    fn pkg(language: Language) -> Project {
        Project::Package(Box::new(MockPackage::new(
            None,
            None,
            &format!("/repo/{language:?}/manifest"),
            &format!("{language:?}/manifest"),
            language,
        )))
    }

    /// Regression: confirms the inlined `.iter().any(...)` implementation
    /// filters exactly the expected subset, byte-identical to the removed
    /// `Vec<Language>` + `Vec::contains` version.
    ///
    /// Fixture is intentionally the "two languages, four projects" case:
    /// input `[Node, Rust]` against `{Python,
    /// Node, Rust, Dart}` must keep exactly the Node and Rust projects,
    /// in their original order (`Vec::retain` is order-preserving).
    #[test]
    fn test_retain_by_language_two_langs_four_projects() {
        let projects = [
            pkg(Language::Python),
            pkg(Language::Node),
            pkg(Language::Rust),
            pkg(Language::Dart),
        ];
        let mut refs: Vec<&Project> = projects.iter().collect();
        retain_by_language(&[CliLanguage::Node, CliLanguage::Rust], &mut refs);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].language(), Language::Node);
        assert_eq!(refs[1].language(), Language::Rust);
    }

    /// Regression: empty `langs` slice is a no-op — every project stays.
    /// This locks the "no `--language` flag = no filter" CLI convention.
    #[test]
    fn test_retain_by_language_empty_langs_is_no_op() {
        let projects = [pkg(Language::Python), pkg(Language::Node)];
        let mut refs: Vec<&Project> = projects.iter().collect();
        retain_by_language(&[], &mut refs);
        assert_eq!(refs.len(), 2);
    }
}
