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
///
/// Filters in-place with an inline `.iter().any(...)` pass over `langs`
/// rather than pre-collecting a transient `Vec<Language>`. Both
/// `CliLanguage` and `Language` are `Copy` enums with sub-word variants,
/// so the intermediate `Vec` was pure heap allocation with no cache win
/// on the tiny `--language` list (typically 1–2 items). Inlining also
/// obviates the `Vec::contains` call, which does the same linear scan the
/// `any` closure now does directly. Behavior is byte-identical because
/// `Vec::contains` and `Iterator::any` both short-circuit on the first
/// match and both iterate `langs` in insertion order.
pub fn retain_by_language(langs: &[CliLanguage], projects: &mut Vec<&Project>) {
    if langs.is_empty() {
        return;
    }
    projects.retain(|project| {
        let lang = project.language();
        langs.iter().any(|&l| Language::from(l) == lang)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use changepacks_core::{Package, UpdateType};
    use rstest::rstest;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

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

    /// Minimal `Package` mock scoped to this test module — only carries
    /// the `language: Language` needed by `retain_by_language`. Mirrors
    /// the same-shape mock already used in `filter_options.rs`; kept
    /// local so the test does not add a workspace-visible test util.
    #[derive(Debug)]
    struct MockPackage {
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        dependencies: HashSet<String>,
    }

    #[async_trait]
    impl Package for MockPackage {
        fn name(&self) -> Option<&str> {
            None
        }
        fn version(&self) -> Option<&str> {
            None
        }
        fn path(&self) -> &Path {
            &self.path
        }
        fn relative_path(&self) -> &Path {
            &self.relative_path
        }
        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_changed(&self) -> bool {
            false
        }
        fn language(&self) -> Language {
            self.language
        }
        fn dependencies(&self) -> &HashSet<String> {
            &self.dependencies
        }
        fn add_dependency(&mut self, dep: &str) {
            self.dependencies.insert(dep.to_string());
        }
        fn set_changed(&mut self, _changed: bool) {}
        fn default_publish_command(&self) -> String {
            String::new()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            None
        }
    }

    fn pkg(language: Language) -> Project {
        Project::Package(Box::new(MockPackage {
            path: PathBuf::from(format!("/repo/{language:?}/manifest")),
            relative_path: PathBuf::from(format!("{language:?}/manifest")),
            language,
            dependencies: HashSet::new(),
        }))
    }

    /// Regression: confirms the inlined `.iter().any(...)` implementation
    /// filters exactly the expected subset, byte-identical to the removed
    /// `Vec<Language>` + `Vec::contains` version.
    ///
    /// Fixture is intentionally the "two languages, four projects" case
    /// called out by the batch plan: input `[Node, Rust]` against `{Python,
    /// Node, Rust, Dart}` must keep exactly the Node and Rust projects,
    /// in their original order (`Vec::retain` is order-preserving).
    #[test]
    fn test_retain_by_language_two_langs_four_projects() {
        let projects = vec![
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
        let projects = vec![pkg(Language::Python), pkg(Language::Node)];
        let mut refs: Vec<&Project> = projects.iter().collect();
        retain_by_language(&[], &mut refs);
        assert_eq!(refs.len(), 2);
    }
}
