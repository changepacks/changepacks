use colored::Colorize;
use std::fmt::Display;

/// Supported programming languages and their corresponding package manager ecosystems.
///
/// Each variant represents a language that changepacks can manage versions for.
///
/// # Declaration order is a user-visible contract
///
/// The derived `Ord` follows declaration order, and `cmp_lang_then_name` in
/// `crate::project` compares the language FIRST when ordering projects. So the
/// order of the variants below is the primary sort key for the grouping of
/// `changepacks check` output and for the `changepacks publish` listing order.
/// Do not alphabetize these variants and do not insert a new language in the
/// "obvious" alphabetical spot: append it instead, or accept that the CLI output
/// order changes. `test_language_declaration_order_is_the_sort_contract` pins
/// this.
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
    pub const fn publish_key(self) -> &'static str {
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
    #[case(Language::Node, "Node.js")]
    #[case(Language::Rust, "Rust")]
    #[case(Language::Dart, "Dart")]
    #[case(Language::CSharp, "C#")]
    #[case(Language::Java, "Java")]
    fn test_language_display(#[case] language: Language, #[case] expected: &str) {
        let display = format!("{language}");
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

    /// The derived `Ord` follows declaration order and `project::cmp_lang_then_name`
    /// compares the language first, so this array IS the user-visible grouping order
    /// of `changepacks check` and the `changepacks publish` listing. Reordering the
    /// enum must fail here rather than silently reshuffle CLI output.
    #[test]
    fn test_language_declaration_order_is_the_sort_contract() {
        let declared = [
            Language::Python,
            Language::Node,
            Language::Rust,
            Language::Dart,
            Language::CSharp,
            Language::Java,
        ];
        assert!(
            declared.is_sorted(),
            "Language declaration order changed; it is the primary sort key used by \
             project::cmp_lang_then_name for CLI output"
        );

        let mut shuffled = [
            Language::Java,
            Language::Dart,
            Language::Python,
            Language::CSharp,
            Language::Node,
            Language::Rust,
        ];
        shuffled.sort_unstable();
        assert_eq!(shuffled, declared);
    }
}
