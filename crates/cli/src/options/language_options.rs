use changepacks_core::Language;
use clap::ValueEnum;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliLanguage {
    Python,
    Node,
    Rust,
    Dart,
}

impl From<CliLanguage> for Language {
    fn from(value: CliLanguage) -> Self {
        match value {
            CliLanguage::Python => Language::Python,
            CliLanguage::Node => Language::Node,
            CliLanguage::Rust => Language::Rust,
            CliLanguage::Dart => Language::Dart,
        }
    }
}
