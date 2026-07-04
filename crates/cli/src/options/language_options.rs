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

/// Retain only projects whose language matches one of `langs`.
///
/// No-op when `langs` is empty (the CLI convention: an empty `--language`
/// flag list means "no language filter applied"). Extracted from the
/// identical filter block that was previously open-coded in `check`,
/// `publish`, and `changepacks` — one place to evolve the semantics
/// (e.g. accept a `Language::Java` alias for `--language kotlin`).
pub fn retain_by_language(langs: &[CliLanguage], projects: &mut Vec<&Project>) {
    if langs.is_empty() {
        return;
    }
    let allowed: Vec<Language> = langs.iter().map(|&l| Language::from(l)).collect();
    projects.retain(|project| allowed.contains(&project.language()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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
}
