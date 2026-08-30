use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
};

use changepacks_core::{Language, Project};

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
    language: Language,
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
///
/// # The declaring manifest's own ecosystem wins
///
/// Two lookups back every resolution: one keyed by the declaring manifest's
/// [`Language`] plus the name, and one keyed by the name alone. The
/// same-ecosystem index is consulted first, and the name-only index is the
/// fallback when that ecosystem carries no such name.
///
/// That ordering is what lets both real shapes coexist:
///
/// - A **bridge package** names a sibling from another ecosystem — a
///   `package.json` declaring `"core": "workspace:*"` against a `core` crate.
///   No Node project is called `core`, so the fallback resolves it.
/// - A **polyglot product** ships one library as a crate, an npm package AND a
///   wheel under one name. Every manifest that depends on it is itself one of
///   those ecosystems, so the same-ecosystem lookup answers first and the three
///   same-named siblings never compete. A name-only index called this
///   ambiguous and refused to order the workspace at all.
///
/// A name therefore stays [`Ambiguous`](ProjectNameResolution::Ambiguous) only
/// when the lookup that actually answered it found several carriers: two
/// projects of the declaring ecosystem sharing a name, or — with none there —
/// several cross-ecosystem carriers to fall back onto.
pub struct ProjectNameAnalysis<'a> {
    by_language: HashMap<(Language, &'a str), Option<usize>>,
    by_name: HashMap<&'a str, Option<usize>>,
    referenced_ambiguity: Option<ReferencedDependencyAmbiguity<'a>>,
}

/// Record `index` as a carrier of `key`, collapsing the slot to `None` (the
/// "several carriers" marker) once a second project claims it.
///
/// Shared by both indexes in [`ProjectNameAnalysis::new`] so the duplicate
/// marker can never be applied to only one of them.
fn insert_carrier<K: std::hash::Hash + Eq>(
    index_map: &mut HashMap<K, Option<usize>>,
    key: K,
    index: usize,
) -> bool {
    match index_map.entry(key) {
        Entry::Occupied(entry) => {
            *entry.into_mut() = None;
            true
        }
        Entry::Vacant(entry) => {
            entry.insert(Some(index));
            false
        }
    }
}

/// The resolution rule itself, as a free function so [`ProjectNameAnalysis::new`]
/// can apply it to the half-built indexes and [`ProjectNameAnalysis::resolve`]
/// can expose it, without the two drifting apart.
fn resolve_in(
    by_language: &HashMap<(Language, &str), Option<usize>>,
    by_name: &HashMap<&str, Option<usize>>,
    language: Language,
    name: &str,
) -> ProjectNameResolution {
    let carriers = by_language
        .get(&(language, name))
        .or_else(|| by_name.get(name));
    match carriers {
        Some(Some(index)) => ProjectNameResolution::Unique(*index),
        Some(None) => ProjectNameResolution::Ambiguous,
        None => ProjectNameResolution::Missing,
    }
}

impl<'a> ProjectNameAnalysis<'a> {
    /// Index `projects` both per ecosystem and by bare name, marking every
    /// duplicated key ambiguous.
    #[must_use]
    pub fn new(projects: &[&'a Project]) -> Self {
        let mut by_language = HashMap::with_capacity(projects.len());
        let mut by_name = HashMap::with_capacity(projects.len());
        // A name duplicated inside one ecosystem is necessarily duplicated
        // across all of them, so the name-only index alone decides whether any
        // duplicate exists at all.
        let mut has_duplicate_names = false;
        for (index, project) in projects.iter().enumerate() {
            if let Some(name) = project.name() {
                insert_carrier(&mut by_language, (project.language(), name), index);
                has_duplicate_names |= insert_carrier(&mut by_name, name, index);
            }
        }

        // `insert_carrier`'s `Entry::Occupied` arm is the only place a slot ever
        // becomes `None`, so when no project name is duplicated no key maps to
        // `None` and no dependency below can resolve `Ambiguous`. The guarded
        // scan therefore provably cannot set `ambiguous`, and skipping it leaves
        // the output byte-identical for every input while dropping an
        // O(projects x dependencies) hash lookup from the common no-duplicate
        // case.
        //
        // The reported pair is ordered by NAME first so that a workspace whose
        // only duplicate is single-language reports exactly what it always did;
        // the language is a tiebreak for the rare repo that duplicates the same
        // name in two ecosystems at once.
        let mut ambiguous = None;
        if has_duplicate_names {
            for project in projects {
                let language = project.language();
                for candidate in project.dependencies() {
                    let candidate = candidate.as_str();
                    if resolve_in(&by_language, &by_name, language, candidate)
                        == ProjectNameResolution::Ambiguous
                        && ambiguous.is_none_or(|(current_name, current_language)| {
                            (candidate, language) < (current_name, current_language)
                        })
                    {
                        ambiguous = Some((candidate, language));
                    }
                }
            }
        }

        let referenced_ambiguity =
            ambiguous.map(|(dependency, language)| ReferencedDependencyAmbiguity {
                language,
                dependency,
                candidates: sorted_candidates(
                    projects,
                    candidate_scope(&by_language, language, dependency),
                    dependency,
                ),
            });

        Self {
            by_language,
            by_name,
            referenced_ambiguity,
        }
    }

    /// Resolve one dependency name as the manifest that declared it means it:
    /// its own ecosystem first, then the cross-ecosystem fallback.
    #[must_use]
    pub fn resolve(&self, language: Language, name: &str) -> ProjectNameResolution {
        resolve_in(&self.by_language, &self.by_name, language, name)
    }

    /// Relative manifest paths of every project a `language` manifest could
    /// have meant by `name`, ordered by [`compare_paths`] so a lossy-colliding
    /// non-UTF-8 pair still reports deterministically. Pass the same slice that
    /// built the analysis.
    ///
    /// The listed carriers are exactly the ones the lookup that answered
    /// consulted: `language`'s own projects when that ecosystem carries the
    /// name, every carrier otherwise.
    ///
    /// A name the index never saw provably has no carrier, so that case skips
    /// the scan entirely. The one name already reported by
    /// [`Self::referenced_ambiguity`] reuses the candidates computed in
    /// [`Self::new`] instead of recomputing them.
    ///
    /// Returns a [`Cow`] so the two cases that already have their answer — the
    /// cached ambiguity and a name with no carrier — hand back a borrowed
    /// slice instead of allocating a `Vec` plus one `PathBuf` per carrier that
    /// every caller only reads.
    #[must_use]
    pub fn candidates_for(
        &self,
        projects: &[&Project],
        language: Language,
        name: &str,
    ) -> Cow<'_, [PathBuf]> {
        // `new` stored the same `sorted_candidates` call for the reported
        // ambiguity, and the doc contract above requires callers to pass the
        // same slice, so for that single name the stored vector is provably
        // what the fall-through below would recompute. Reusing it drops one
        // O(projects) filter plus one sort per ambiguity report and leaves a
        // single source of truth for the reported candidate order.
        if let Some(ambiguity) = &self.referenced_ambiguity
            && ambiguity.language == language
            && ambiguity.dependency() == name
        {
            return Cow::Borrowed(ambiguity.candidates());
        }
        if self.resolve(language, name) == ProjectNameResolution::Missing {
            return Cow::Borrowed(&[]);
        }
        Cow::Owned(sorted_candidates(
            projects,
            candidate_scope(&self.by_language, language, name),
            name,
        ))
    }

    pub(crate) const fn referenced_ambiguity(&self) -> Option<&ReferencedDependencyAmbiguity<'a>> {
        self.referenced_ambiguity.as_ref()
    }
}

/// Which carriers a `language` manifest's `name` lookup consulted:
/// `Some(language)` when that ecosystem carries the name, `None` when the
/// cross-ecosystem fallback answered instead.
fn candidate_scope(
    by_language: &HashMap<(Language, &str), Option<usize>>,
    language: Language,
    name: &str,
) -> Option<Language> {
    by_language
        .contains_key(&(language, name))
        .then_some(language)
}

fn sorted_candidates(projects: &[&Project], scope: Option<Language>, name: &str) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = projects
        .iter()
        .filter(|project| {
            scope.is_none_or(|language| project.language() == language)
                && project.name() == Some(name)
        })
        .map(|project| project.relative_path().to_path_buf())
        .collect();
    // Unstable sort: `compare_paths` is a total order over the whole element
    // (a bare `PathBuf`), reporting `Equal` only for byte-identical paths, so
    // `Equal` elements are indistinguishable and stability is unobservable.
    // Carriers are distinct projects with distinct manifest paths anyway.
    // Unlike the stable sort, this allocates no scratch buffer.
    candidates.sort_unstable_by(|left, right| compare_paths(left, right));
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

    use changepacks_core::{Language, Project};
    use changepacks_node::package::NodePackage;

    use crate::test_support::{create_project, create_project_at, create_project_in};

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
        assert_eq!(
            analysis.resolve(Language::Node, "core"),
            ProjectNameResolution::Unique(0)
        );
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
            analysis
                .candidates_for(&projects, Language::Node, "shared")
                .as_ref(),
            [
                PathBuf::from("alpha/package.json"),
                PathBuf::from("zeta/package.json"),
            ]
            .as_slice()
        );
        assert_eq!(
            analysis
                .candidates_for(&projects, Language::Node, "app")
                .as_ref(),
            [PathBuf::from("app/package.json")].as_slice()
        );
        assert!(
            analysis
                .candidates_for(&projects, Language::Node, "external")
                .is_empty()
        );
    }

    #[test]
    fn candidates_for_matches_the_candidates_stored_on_the_referenced_ambiguity() {
        // Given
        let mut shared_zeta = create_project("zeta", vec![]);
        shared_zeta.set_name("shared".to_string());
        let mut shared_alpha = create_project("alpha", vec![]);
        shared_alpha.set_name("shared".to_string());
        let app = create_project("app", vec!["shared"]);
        let projects = [&shared_zeta, &app, &shared_alpha];
        let analysis = ProjectNameAnalysis::new(&projects);
        let reported = analysis
            .referenced_ambiguity()
            .expect("the referenced duplicate must be ambiguous");

        // When
        let candidates = analysis.candidates_for(&projects, Language::Node, reported.dependency());

        // Then
        assert_eq!(candidates.as_ref(), reported.candidates());
        assert_eq!(
            candidates.as_ref(),
            [
                PathBuf::from("alpha/package.json"),
                PathBuf::from("zeta/package.json"),
            ]
            .as_slice()
        );
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
        let analysis = ProjectNameAnalysis::new(&projects);
        let candidates = analysis.candidates_for(&projects, Language::Node, "shared");

        // Then
        assert_eq!(candidates.as_ref(), [left_path, right_path].as_slice());
    }

    /// One product published as a crate, an npm package and a wheel under a
    /// single name is the shape every polyglot monorepo has. Each manifest's
    /// dependency edge names its OWN ecosystem's package, so all three resolve
    /// uniquely and nothing is ambiguous.
    #[test]
    fn resolves_same_name_siblings_across_languages_independently() {
        // Given
        let rust_lib =
            create_project_in("shared", Language::Rust, "libs/shared/Cargo.toml", vec![]);
        let node_lib = create_project_in(
            "shared",
            Language::Node,
            "packages/node/package.json",
            vec![],
        );
        let python_lib = create_project_in(
            "shared",
            Language::Python,
            "packages/python/pyproject.toml",
            vec![],
        );
        let rust_app =
            create_project_in("app", Language::Rust, "apps/cli/Cargo.toml", vec!["shared"]);
        let node_app = create_project_in(
            "app",
            Language::Node,
            "apps/web/package.json",
            vec!["shared"],
        );
        let projects = [&rust_lib, &node_lib, &python_lib, &rust_app, &node_app];

        // When
        let analysis = ProjectNameAnalysis::new(&projects);

        // Then
        assert!(analysis.referenced_ambiguity().is_none());
        assert_eq!(
            analysis.resolve(Language::Rust, "shared"),
            ProjectNameResolution::Unique(0)
        );
        assert_eq!(
            analysis.resolve(Language::Node, "shared"),
            ProjectNameResolution::Unique(1)
        );
        assert_eq!(
            analysis.resolve(Language::Python, "shared"),
            ProjectNameResolution::Unique(2)
        );
        // A Dart manifest carries no `shared` of its own, so it falls back
        // across ecosystems — and there it genuinely cannot tell the three
        // siblings apart.
        assert_eq!(
            analysis.resolve(Language::Dart, "shared"),
            ProjectNameResolution::Ambiguous
        );
        assert_eq!(
            analysis.resolve(Language::Rust, "external"),
            ProjectNameResolution::Missing
        );
    }

    /// The bridge shape: a `package.json` depending on a name only a crate
    /// carries. Nothing in the Node ecosystem answers, so the cross-ecosystem
    /// fallback resolves it — that is how `updateOn`-free bridge packages get
    /// their reverse-dependency bump.
    #[test]
    fn resolves_cross_language_dependency_when_the_declaring_ecosystem_has_no_carrier() {
        // Given
        let rust_core = create_project_in("core", Language::Rust, "crates/core/Cargo.toml", vec![]);
        let node_bridge = create_project_in(
            "bridge",
            Language::Node,
            "bridge/package.json",
            vec!["core"],
        );
        let projects = [&rust_core, &node_bridge];

        // When
        let analysis = ProjectNameAnalysis::new(&projects);

        // Then
        assert!(analysis.referenced_ambiguity().is_none());
        assert_eq!(
            analysis.resolve(Language::Node, "core"),
            ProjectNameResolution::Unique(0)
        );
    }

    /// Duplicates inside ONE ecosystem stay ambiguous, and the reported
    /// candidates list only that ecosystem's carriers — a same-named sibling in
    /// another language is not a candidate for a name it can never satisfy.
    #[test]
    fn reports_only_same_language_carriers_for_a_duplicated_name() {
        // Given
        let node_zeta = create_project_in("shared", Language::Node, "zeta/package.json", vec![]);
        let node_alpha = create_project_in("shared", Language::Node, "alpha/package.json", vec![]);
        let rust_lib =
            create_project_in("shared", Language::Rust, "libs/shared/Cargo.toml", vec![]);
        let node_app = create_project_in("app", Language::Node, "app/package.json", vec!["shared"]);
        let projects = [&node_zeta, &node_alpha, &rust_lib, &node_app];

        // When
        let analysis = ProjectNameAnalysis::new(&projects);
        let ambiguity = analysis
            .referenced_ambiguity()
            .expect("the duplicated Node name is referenced by a Node manifest");

        // Then
        assert_eq!(ambiguity.dependency(), "shared");
        assert_eq!(
            ambiguity.candidates(),
            [
                PathBuf::from("alpha/package.json"),
                PathBuf::from("zeta/package.json"),
            ]
        );
        assert_eq!(
            analysis.resolve(Language::Rust, "shared"),
            ProjectNameResolution::Unique(2)
        );
        assert_eq!(
            analysis
                .candidates_for(&projects, Language::Rust, "shared")
                .as_ref(),
            [PathBuf::from("libs/shared/Cargo.toml")].as_slice()
        );
    }

    #[test]
    fn resolves_duplicate_and_ignores_nameless_projects() {
        // Given
        let mut shared_alpha = create_project("alpha", vec![]);
        shared_alpha.set_name("shared".to_string());
        let mut shared_zeta = create_project("zeta", vec![]);
        shared_zeta.set_name("shared".to_string());
        let nameless_alpha = create_project_at(None, "nameless-alpha/package.json");
        let nameless_zeta = create_project_at(None, "nameless-zeta/package.json");

        // When
        let analysis = ProjectNameAnalysis::new(&[
            &shared_zeta,
            &nameless_alpha,
            &shared_alpha,
            &nameless_zeta,
        ]);

        // Then
        assert_eq!(
            analysis.resolve(Language::Node, "shared"),
            ProjectNameResolution::Ambiguous
        );
        assert_eq!(
            analysis.resolve(Language::Node, "nameless-alpha"),
            ProjectNameResolution::Missing
        );
        assert_eq!(
            analysis.resolve(Language::Node, "nameless-zeta"),
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
