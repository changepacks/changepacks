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
    /// C# projects using .csproj (`NuGet`, `dotnet`)
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

    /// Returns the flag that converts a publish command into its dry-run form.
    ///
    /// Returns `None` for ecosystems whose default publish tooling does not
    /// support a built-in dry-run mode. In that case the user should configure
    /// `publishDryRun` in `.changepacks/config.json` to provide a custom dry-run
    /// command for the affected project or language.
    ///
    /// Defaults assume:
    /// - Node (npm/pnpm/yarn/bun): `--dry-run`
    /// - Python (uv): `--dry-run`
    /// - Rust (cargo): `--dry-run`
    /// - Dart (`dart pub`): `--dry-run`
    /// - Java (Gradle): `--dry-run`
    /// - C# (`dotnet nuget push`): unsupported (`None`)
    #[must_use]
    pub const fn dry_run_flag(&self) -> Option<&'static str> {
        match self {
            Self::Node | Self::Python | Self::Rust | Self::Dart | Self::Java => Some("--dry-run"),
            Self::CSharp => None,
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

    #[rstest]
    #[case(Language::Python, "python")]
    #[case(Language::Node, "node")]
    #[case(Language::Rust, "rust")]
    #[case(Language::Dart, "dart")]
    #[case(Language::CSharp, "csharp")]
    #[case(Language::Java, "java")]
    fn test_publish_key(#[case] language: Language, #[case] expected: &str) {
        assert_eq!(language.publish_key(), expected);
    }

    #[rstest]
    #[case(Language::Python, Some("--dry-run"))]
    #[case(Language::Node, Some("--dry-run"))]
    #[case(Language::Rust, Some("--dry-run"))]
    #[case(Language::Dart, Some("--dry-run"))]
    #[case(Language::Java, Some("--dry-run"))]
    #[case(Language::CSharp, None)]
    fn test_dry_run_flag(#[case] language: Language, #[case] expected: Option<&str>) {
        assert_eq!(language.dry_run_flag(), expected);
    }
}
