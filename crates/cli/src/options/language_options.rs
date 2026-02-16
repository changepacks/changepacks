use changepacks_core::Language;
use clap::ValueEnum;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliLanguage {
    Python,
    Node,
    Rust,
    Dart,
    Java,
}

impl From<CliLanguage> for Language {
    fn from(value: CliLanguage) -> Self {
        match value {
            CliLanguage::Python => Self::Python,
            CliLanguage::Node => Self::Node,
            CliLanguage::Rust => Self::Rust,
            CliLanguage::Dart => Self::Dart,
            CliLanguage::Java => Self::Java,
        }
    }
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
    fn test_cli_language_to_language(#[case] cli_lang: CliLanguage, #[case] expected: Language) {
        let result: Language = cli_lang.into();
        assert_eq!(result, expected);
    }
}
