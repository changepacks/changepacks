use changepacks_core::{Language, Project};
use clap::ValueEnum;

/// The `--language` flag's accepted values, one per [`Language`].
///
/// # Declaration order mirrors [`Language`]
///
/// clap renders `ValueEnum` values in declaration order, so these variants are
/// what `--help` lists. [`Language`] documents its own declaration order as a
/// user-visible sort contract (it is the primary key of `check` grouping and
/// `publish` listing order), so the two orders are kept identical: `--help`
/// must not advertise a different language order than the commands print.
/// `test_cli_language_value_variants_follow_language_order` pins this.
///
/// The variant *names* are the source of the accepted flag value strings: clap
/// kebab-cases them, so `CSharp` is spelled `--language c-sharp` (NOT `csharp`,
/// which is the unrelated `Language::publish_key` config key). Renaming a
/// variant is therefore a breaking CLI change.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliLanguage {
    Python,
    Node,
    Rust,
    Dart,
    CSharp,
    Java,
}

impl From<CliLanguage> for Language {
    fn from(value: CliLanguage) -> Self {
        match value {
            CliLanguage::Python => Self::Python,
            CliLanguage::Node => Self::Node,
            CliLanguage::Rust => Self::Rust,
            CliLanguage::Dart => Self::Dart,
            CliLanguage::CSharp => Self::CSharp,
            CliLanguage::Java => Self::Java,
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
///
/// The parameter order deliberately mirrors its superset sibling
/// [`retain_by_filters`](super::filter_options::retain_by_filters): the
/// `&mut Vec<&Project>` being retained comes first, the selection criteria
/// after. The two are chosen between per command (`publish` has no `--filter`
/// flag and uses this one), so a transposed order would be a standing
/// argument-swap trap.
pub fn retain_by_language(projects: &mut Vec<&Project>, langs: &[CliLanguage]) {
    if langs.is_empty() {
        return;
    }
    projects.retain(|project| language_slice_contains(langs, project.language()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    use changepacks_core::test_support::MockPackage;

    #[rstest]
    #[case(CliLanguage::Python, Language::Python)]
    #[case(CliLanguage::Node, Language::Node)]
    #[case(CliLanguage::Rust, Language::Rust)]
    #[case(CliLanguage::Dart, Language::Dart)]
    #[case(CliLanguage::CSharp, Language::CSharp)]
    #[case(CliLanguage::Java, Language::Java)]
    fn test_cli_language_to_language(#[case] cli_lang: CliLanguage, #[case] expected: Language) {
        let result: Language = cli_lang.into();
        assert_eq!(result, expected);
    }

    /// Pins the *other* direction of the mapping: every [`Language`] must be
    /// reachable from some [`CliLanguage`].
    ///
    /// `From<CliLanguage> for Language` matches on `CliLanguage`, so adding a
    /// seventh `Language` variant compiles cleanly while `--language` silently
    /// cannot select it — and both [`retain_by_language`] and
    /// `filter_update_map_by_language` would then drop every project of that
    /// language instead of filtering it. The local exhaustive match below turns
    /// that silent gap into a compile error (E0004), and the round trip through
    /// `Language::from` proves the two directions agree rather than merely that
    /// some arm exists.
    #[test]
    fn test_every_language_has_a_cli_language() {
        const fn cli_language_for(language: Language) -> CliLanguage {
            // Exhaustive on purpose: do NOT add a `_ =>` arm. A new `Language`
            // variant must fail to compile here until `CliLanguage` gains a
            // matching variant.
            match language {
                Language::Python => CliLanguage::Python,
                Language::Node => CliLanguage::Node,
                Language::Rust => CliLanguage::Rust,
                Language::Dart => CliLanguage::Dart,
                Language::CSharp => CliLanguage::CSharp,
                Language::Java => CliLanguage::Java,
            }
        }

        for language in [
            Language::Python,
            Language::Node,
            Language::Rust,
            Language::Dart,
            Language::CSharp,
            Language::Java,
        ] {
            assert_eq!(
                Language::from(cli_language_for(language)),
                language,
                "CliLanguage round trip disagrees for {language:?}"
            );
        }
    }

    /// clap lists `ValueEnum` values in declaration order, and [`Language`]'s
    /// declaration order is the documented sort contract behind `check`
    /// grouping and `publish` listing. So `--help` must advertise the languages
    /// in exactly that order: mapping `value_variants` through `Language::from`
    /// has to come out already sorted (derived `Ord` = declaration order).
    /// Reordering `CliLanguage` away from `Language` fails here.
    #[test]
    fn test_cli_language_value_variants_follow_language_order() {
        let ordered: Vec<Language> = CliLanguage::value_variants()
            .iter()
            .map(|&cli_lang| Language::from(cli_lang))
            .collect();

        assert_eq!(
            ordered.len(),
            6,
            "a CliLanguage variant was added or removed without updating this test"
        );
        assert!(
            ordered.is_sorted(),
            "CliLanguage declaration order diverged from Language declaration order, so \
             --help would list languages in a different order than check/publish print \
             them: {ordered:?}"
        );
    }

    fn pkg(language: Language) -> Project {
        Project::Package(Box::new(MockPackage::with_all(
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
        retain_by_language(&mut refs, &[CliLanguage::Node, CliLanguage::Rust]);
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
        retain_by_language(&mut refs, &[]);
        assert_eq!(refs.len(), 2);
    }
}
