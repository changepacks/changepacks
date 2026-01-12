use colored::Colorize;
use std::fmt::Display;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Language {
    Python,
    Node,
    Rust,
    Dart,
    CSharp,
}

impl Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Language::Python => "Python".yellow().bold(),
                Language::Node => "Node.js".green().bold(),
                Language::Rust => "Rust".truecolor(139, 69, 19).bold(),
                Language::Dart => "Dart".blue().bold(),
                Language::CSharp => "C#".magenta().bold(),
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(Language::Python, "Python")]
    #[case(Language::Node, "Node")]
    #[case(Language::Rust, "Rust")]
    #[case(Language::Dart, "Dart")]
    #[case(Language::CSharp, "C#")]
    fn test_language_display(#[case] language: Language, #[case] expected: &str) {
        let display = format!("{}", language);
        assert!(display.contains(expected));
    }
}
