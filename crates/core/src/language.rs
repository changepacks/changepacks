use colored::Colorize;
use std::fmt::Display;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Language {
    Python,
    Node,
    Rust,
    Dart,
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
            }
        )
    }
}
