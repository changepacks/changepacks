use std::{
    cmp::Ordering,
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
};

use changepacks_core::Project;

/// Outcome of resolving a dependency name against the discovered project set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectNameResolution {
    /// No discovered project carries this name (an external dependency).
    Missing,
    /// More than one discovered project carries this name.
    Ambiguous,
    /// Exactly one project carries this name, at the given index in the
    /// slice that built the analysis.
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

/// Name-to-project index shared by every consumer that has to turn a
/// dependency name into the project that provides it: `sort_by_dependencies`,
/// `apply_reverse_dependencies` and the CLI `check --tree` renderer.
pub struct ProjectNameAnalysis<'a> {
    name_to_index: HashMap<&'a str, Option<usize>>,
    referenced_ambiguity: Option<ReferencedDependencyAmbiguity<'a>>,
}

impl<'a> ProjectNameAnalysis<'a> {
    /// Index `projects` by name, marking every duplicated name ambiguous.
    #[must_use]
    pub fn new(projects: &[&'a Project]) -> Self {
        let mut name_to_index = HashMap::with_capacity(projects.len());
        let mut has_duplicate_names = false;
        for (index, project) in projects.iter().enumerate() {
            if let Some(name) = project.name() {
                match name_to_index.entry(name) {
                    Entry::Occupied(entry) => {
                        has_duplicate_names = true;
                        *entry.into_mut() = None;
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(Some(index));
                    }
                }
            }
        }

        // The `Entry::Occupied` arm above is the only place a map value ever
        // becomes `None`, so when no project name is duplicated no key maps to
        // `None` and `name_to_index.get(candidate) == Some(&None)` below is
        // unsatisfiable. The guarded scan therefore provably cannot set
        // `dependency`, and skipping it leaves the output byte-identical for
        // every input while dropping an O(projects x dependencies) hash lookup
        // from the common no-duplicate case.
        let mut dependency = None;
        if has_duplicate_names {
            for project in projects {
                for candidate in project.dependencies() {
                    if name_to_index.get(candidate.as_str()) == Some(&None)
                        && dependency.is_none_or(|current| candidate.as_str() < current)
                    {
                        dependency = Some(candidate.as_str());
                    }
                }
            }
        }

        let referenced_ambiguity = dependency.map(|dependency| ReferencedDependencyAmbiguity {
            dependency,
            candidates: sorted_candidates(projects, dependency),
        });

        Self {
            name_to_index,
            referenced_ambiguity,
        }
    }

    /// Resolve one dependency name against the indexed projects.
    #[must_use]
    pub fn resolve(&self, name: &str) -> ProjectNameResolution {
        match self.name_to_index.get(name) {
            Some(Some(index)) => ProjectNameResolution::Unique(*index),
            Some(None) => ProjectNameResolution::Ambiguous,
            None => ProjectNameResolution::Missing,
        }
    }

    /// Relative manifest paths of every project carrying `name`, ordered by
    /// [`compare_paths`] so a lossy-colliding non-UTF-8 pair still reports
    /// deterministically. Pass the same slice that built the analysis.
    ///
    /// A name the index never saw provably has no carrier, so that case skips
    /// the scan entirely.
    #[must_use]
    pub fn candidates_for(&self, projects: &[&Project], name: &str) -> Vec<PathBuf> {
        if self.resolve(name) == ProjectNameResolution::Missing {
            return Vec::new();
        }
        sorted_candidates(projects, name)
    }

    pub(crate) const fn referenced_ambiguity(&self) -> Option<&ReferencedDependencyAmbiguity<'a>> {
        self.referenced_ambiguity.as_ref()
    }
}

fn sorted_candidates(projects: &[&Project], name: &str) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = projects
        .iter()
        .filter(|project| project.name() == Some(name))
        .map(|project| project.relative_path().to_path_buf())
        .collect();
    candidates.sort_by(|left, right| compare_paths(left, right));
    candidates
}

pub(crate) fn compare_paths(left: &Path, right: &Path) -> Ordering {
    changepacks_core::cmp_normalized_paths(left, right)
        .then_with(|| left.as_os_str().cmp(right.as_os_str()))
}

#[cfg(test)]
mod tests {
    use std::{
        cmp::Ordering,
        ffi::OsString,
        path::{Path, PathBuf},
    };

    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    #[cfg(windows)]
    use std::os::windows::ffi::OsStringExt;

    use changepacks_core::Project;
    use changepacks_node::package::NodePackage;

    use crate::test_support::create_project;

    use super::{ProjectNameAnalysis, ProjectNameResolution, compare_paths};

    #[cfg(unix)]
    fn lossy_collision_paths() -> (PathBuf, PathBuf) {
        (
            PathBuf::from(OsString::from_vec(vec![b'p', 0x80, b'/', b'a'])),
            PathBuf::from(OsString::from_vec(vec![b'p', 0x81, b'/', b'a'])),
        )
    }

    #[cfg(windows)]
    fn lossy_collision_paths() -> (PathBuf, PathBuf) {
        (
            PathBuf::from(OsString::from_wide(&[
                b'p'.into(),
                0xD800,
                b'/'.into(),
                b'a'.into(),
            ])),
            PathBuf::from(OsString::from_wide(&[
                b'p'.into(),
                0xD801,
                b'/'.into(),
                b'a'.into(),
            ])),
        )
    }

    #[test]
    fn compare_paths_breaks_normalized_separator_tie_with_original_text() {
        let slash_path = Path::new("packages/a/package.json");
        let backslash_path = Path::new(r"packages\a\package.json");

        assert_eq!(compare_paths(slash_path, backslash_path), Ordering::Less);
        assert_eq!(compare_paths(backslash_path, slash_path), Ordering::Greater);
    }

    #[test]
    fn compare_paths_breaks_lossy_non_unicode_tie_with_raw_os_string() {
        let (left, right) = lossy_collision_paths();

        assert_eq!(left.to_string_lossy(), right.to_string_lossy());
        assert_eq!(compare_paths(&left, &right), Ordering::Less);
        assert_eq!(compare_paths(&right, &left), Ordering::Greater);
    }

    #[test]
    fn reports_lossy_colliding_candidates_deterministically_across_discovery_permutations() {
        // Given
        let (left_path, right_path) = lossy_collision_paths();
        let left = Project::Package(Box::new(NodePackage::new(
            Some("shared".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/left/package.json"),
            left_path.clone(),
        )));
        let right = Project::Package(Box::new(NodePackage::new(
            Some("shared".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/right/package.json"),
            right_path.clone(),
        )));
        let app = create_project("app", vec!["shared"]);
        let permutations = [vec![&right, &app, &left], vec![&left, &right, &app]];

        // When
        let diagnostics: Vec<_> = permutations
            .iter()
            .map(|projects| {
                ProjectNameAnalysis::new(projects)
                    .referenced_ambiguity()
                    .expect("the referenced duplicate must be ambiguous")
                    .candidates()
                    .to_vec()
            })
            .collect();

        // Then
        assert_eq!(diagnostics[0], diagnostics[1]);
        assert_eq!(diagnostics[0], [left_path, right_path]);
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
    fn candidates_for_orders_duplicates_and_stays_empty_for_unknown_names() {
        // Given
        let mut shared_zeta = create_project("zeta", vec![]);
        shared_zeta.set_name("shared".to_string());
        let mut shared_alpha = create_project("alpha", vec![]);
        shared_alpha.set_name("shared".to_string());
        let app = create_project("app", vec!["shared"]);
        let projects = [&shared_zeta, &app, &shared_alpha];

        // When
        let analysis = ProjectNameAnalysis::new(&projects);

        // Then
        assert_eq!(
            analysis.candidates_for(&projects, "shared"),
            [
                PathBuf::from("alpha/package.json"),
                PathBuf::from("zeta/package.json"),
            ]
        );
        assert_eq!(
            analysis.candidates_for(&projects, "app"),
            [PathBuf::from("app/package.json")]
        );
        assert!(analysis.candidates_for(&projects, "external").is_empty());
    }

    #[test]
    fn candidates_for_breaks_lossy_collisions_with_the_shared_path_order() {
        // Given
        let (left_path, right_path) = lossy_collision_paths();
        let left = Project::Package(Box::new(NodePackage::new(
            Some("shared".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/left/package.json"),
            left_path.clone(),
        )));
        let right = Project::Package(Box::new(NodePackage::new(
            Some("shared".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/right/package.json"),
            right_path.clone(),
        )));
        let projects = [&right, &left];

        // When
        let candidates = ProjectNameAnalysis::new(&projects).candidates_for(&projects, "shared");

        // Then
        assert_eq!(candidates, [left_path, right_path]);
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
