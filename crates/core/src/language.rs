use colored::Colorize;
use std::fmt::Display;

/// Supported programming languages and their corresponding package manager ecosystems.
///
/// Each variant represents a language that changepacks can manage versions for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Language {
    /// Python projects using pyproject.toml (pip, uv)
    Python,
    /// Node.js projects using package.json (npm, pnpm, yarn, bun)
    Node,
    /// Rust projects using Cargo.toml (cargo)
    Rust,
    /// Dart projects using pubspec.yaml (pub)
    Dart,
    /// C# projects using .csproj (NuGet, dotnet)
    CSharp,
    /// Java projects using build.gradle or build.gradle.kts (Gradle)
    Java,
}

impl Language {
    /// Returns the config key used for publish command lookup
    #[must_use]
    pub const fn publish_key(&self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Dart => "dart",
            Self::CSharp => "csharp",
            Self::Java => "java",
        }
    }
}

impl Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Python => "Python".yellow().bold(),
                Self::Node => "Node.js".green().bold(),
                Self::Rust => "Rust".truecolor(139, 69, 19).bold(),
                Self::Dart => "Dart".blue().bold(),
                Self::CSharp => "C#".magenta().bold(),
                Self::Java => "Java".red().bold(),
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
    #[case(Language::Java, "Java")]
    fn test_language_display(#[case] language: Language, #[case] expected: &str) {
        let display = format!("{}", language);
        assert!(display.contains(expected));
    }
}
