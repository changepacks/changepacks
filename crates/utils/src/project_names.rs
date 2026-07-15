use std::{
    cmp::Ordering,
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
};

use changepacks_core::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectNameResolution {
    Missing,
    Ambiguous,
    Unique(usize),
}

pub(crate) struct ReferencedDependencyAmbiguity<'a> {
    dependency: &'a str,
    candidates: Vec<PathBuf>,
}

impl<'a> ReferencedDependencyAmbiguity<'a> {
    pub(crate) const fn dependency(&self) -> &'a str {
        self.dependency
    }

    pub(crate) fn candidates(&self) -> &[PathBuf] {
        &self.candidates
    }
}

pub(crate) struct ProjectNameAnalysis<'a> {
    name_to_index: HashMap<&'a str, Option<usize>>,
    referenced_ambiguity: Option<ReferencedDependencyAmbiguity<'a>>,
}

impl<'a> ProjectNameAnalysis<'a> {
    pub(crate) fn new(projects: &[&'a Project]) -> Self {
        let mut name_to_index = HashMap::with_capacity(projects.len());
        for (index, project) in projects.iter().enumerate() {
            if let Some(name) = project.name() {
                match name_to_index.entry(name) {
                    Entry::Occupied(entry) => {
                        *entry.into_mut() = None;
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(Some(index));
                    }
                }
            }
        }

        let mut dependency = None;
        for project in projects {
            for candidate in project.dependencies() {
                if name_to_index.get(candidate.as_str()) == Some(&None)
                    && dependency.is_none_or(|current| candidate.as_str() < current)
                {
                    dependency = Some(candidate.as_str());
                }
            }
        }

        let referenced_ambiguity = dependency.map(|dependency| {
            let mut candidates: Vec<_> = projects
                .iter()
                .filter(|project| project.name() == Some(dependency))
                .map(|project| project.relative_path().to_path_buf())
                .collect();
            candidates.sort_by(|left, right| compare_paths(left, right));
            ReferencedDependencyAmbiguity {
                dependency,
                candidates,
            }
        });

        Self {
            name_to_index,
            referenced_ambiguity,
        }
    }

    pub(crate) fn resolve(&self, name: &str) -> ProjectNameResolution {
        match self.name_to_index.get(name) {
            Some(Some(index)) => ProjectNameResolution::Unique(*index),
            Some(None) => ProjectNameResolution::Ambiguous,
            None => ProjectNameResolution::Missing,
        }
    }

    pub(crate) const fn referenced_ambiguity(&self) -> Option<&ReferencedDependencyAmbiguity<'a>> {
        self.referenced_ambiguity.as_ref()
    }
}

pub(crate) fn compare_paths(left: &Path, right: &Path) -> Ordering {
    let left_lossy = left.to_string_lossy();
    let right_lossy = right.to_string_lossy();

    left_lossy
        .chars()
        .map(|character| if character == '\\' { '/' } else { character })
        .cmp(
            right_lossy
                .chars()
                .map(|character| if character == '\\' { '/' } else { character }),
        )
        .then_with(|| left_lossy.cmp(&right_lossy))
}

#[cfg(test)]
mod tests {
    use std::{
        cmp::Ordering,
        path::{Path, PathBuf},
    };

    use changepacks_core::Project;
    use changepacks_node::package::NodePackage;

    use crate::test_support::create_project;

    use super::{ProjectNameAnalysis, ProjectNameResolution, compare_paths};

    #[test]
    fn compare_paths_breaks_normalized_separator_tie_with_original_text() {
        let slash_path = Path::new("packages/a/package.json");
        let backslash_path = Path::new(r"packages\a\package.json");

        assert_eq!(compare_paths(slash_path, backslash_path), Ordering::Less);
        assert_eq!(compare_paths(backslash_path, slash_path), Ordering::Greater);
    }

    #[cfg(unix)]
    #[test]
    fn compare_paths_preserves_lossy_non_unicode_equality() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let left = PathBuf::from(OsString::from_vec(vec![b'p', 0x80, b'/', b'a']));
        let right = PathBuf::from(OsString::from_vec(vec![b'p', 0x81, b'/', b'a']));

        assert_eq!(compare_paths(&left, &right), Ordering::Equal);
    }

    #[test]
    fn resolves_unique_name_when_dependency_references_one_project() {
        // Given
        let core = create_project("core", vec![]);
        let app = create_project("app", vec!["core"]);

        // When
        let analysis = ProjectNameAnalysis::new(&[&core, &app]);

        // Then
        assert_eq!(analysis.resolve("core"), ProjectNameResolution::Unique(0));
        assert!(analysis.referenced_ambiguity().is_none());
    }

    #[test]
    fn resolves_duplicate_and_ignores_nameless_projects() {
        // Given
        let mut shared_alpha = create_project("alpha", vec![]);
        shared_alpha.set_name("shared".to_string());
        let mut shared_zeta = create_project("zeta", vec![]);
        shared_zeta.set_name("shared".to_string());
        let nameless_alpha = Project::Package(Box::new(NodePackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/nameless-alpha/package.json"),
            PathBuf::from("nameless-alpha/package.json"),
        )));
        let nameless_zeta = Project::Package(Box::new(NodePackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/nameless-zeta/package.json"),
            PathBuf::from("nameless-zeta/package.json"),
        )));

        // When
        let analysis = ProjectNameAnalysis::new(&[
            &shared_zeta,
            &nameless_alpha,
            &shared_alpha,
            &nameless_zeta,
        ]);

        // Then
        assert_eq!(analysis.resolve("shared"), ProjectNameResolution::Ambiguous);
        assert_eq!(
            analysis.resolve("nameless-alpha"),
            ProjectNameResolution::Missing
        );
        assert_eq!(
            analysis.resolve("nameless-zeta"),
            ProjectNameResolution::Missing
        );
        assert!(analysis.referenced_ambiguity().is_none());
    }

    #[test]
    fn reports_referenced_duplicate_deterministically_when_discovery_order_changes() {
        // Given
        let mut shared_zeta = create_project("zeta", vec![]);
        shared_zeta.set_name("shared".to_string());
        let mut shared_alpha = create_project("alpha", vec![]);
        shared_alpha.set_name("shared".to_string());
        let mut zulu_zeta = create_project("zulu-zeta", vec![]);
        zulu_zeta.set_name("zulu".to_string());
        let mut zulu_alpha = create_project("zulu-alpha", vec![]);
        zulu_alpha.set_name("zulu".to_string());
        let app = create_project("app", vec!["zulu", "shared"]);
        let permutations = [
            vec![&shared_zeta, &zulu_alpha, &app, &shared_alpha, &zulu_zeta],
            vec![&zulu_zeta, &shared_alpha, &shared_zeta, &app, &zulu_alpha],
        ];

        // When
        let diagnostics: Vec<_> = permutations
            .iter()
            .map(|projects| {
                let analysis = ProjectNameAnalysis::new(projects);
                let ambiguity = analysis
                    .referenced_ambiguity()
                    .expect("the referenced duplicate must be ambiguous");
                (ambiguity.dependency(), ambiguity.candidates().to_vec())
            })
            .collect();

        // Then
        assert_eq!(diagnostics[0], diagnostics[1]);
        assert_eq!(diagnostics[0].0, "shared");
        assert_eq!(
            diagnostics[0].1,
            [
                PathBuf::from("alpha/package.json"),
                PathBuf::from("zeta/package.json"),
            ]
        );
    }
}
