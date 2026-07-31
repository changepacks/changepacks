//! Reverse-dependency expansion for a scheduled update map.
//!
//! Split out of `gen_update_map` so that module keeps a single public entry
//! point, per the one-utility-per-file convention of this crate.

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    hash::BuildHasher,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use changepacks_core::{ChangePackResultLog, Project, UpdateType};

use crate::{
    DependencyAmbiguityError, get_relative_path_ref,
    project_names::{ProjectNameAnalysis, ProjectNameResolution, compare_paths},
};

/// Apply reverse dependency updates: if package A depends on package B (via a local workspace dependency),
/// and B is being updated, then A should also be updated as PATCH.
///
/// Every already-scheduled path in `update_map` is treated as an expansion seed. Callers holding an
/// [`UpdatePlan`](crate::UpdatePlan) must use
/// [`UpdatePlan::apply_reverse_dependencies`](crate::UpdatePlan::apply_reverse_dependencies)
/// instead, so that generated provenance and the narrower seed set are preserved.
///
/// # Errors
/// Returns an error when project names are ambiguous or a project path is outside the repo.
pub fn apply_reverse_dependencies<S: BuildHasher>(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>), S>,
    projects: &[&Project],
    repo_root_path: &Path,
) -> Result<()> {
    apply_reverse_dependencies_with_provenance(
        update_map,
        projects,
        ReverseDependencyContext {
            repo_root_path,
            expansion_seeds: None,
        },
    )
    .map(|_| ())
}

pub(crate) struct ReverseDependencyContext<'a> {
    pub(crate) repo_root_path: &'a Path,
    /// Paths allowed to seed reverse-dependency expansion.
    ///
    /// `None` means "every key of `update_map` is a seed", which is exactly what the free
    /// [`apply_reverse_dependencies`] wants. Spelling that as `Some(update_map.keys().cloned())`
    /// would allocate a whole `HashSet` and clone every `PathBuf` only to compare the map's keys
    /// against a byte-for-byte copy of themselves, so the unrestricted case is encoded in the type
    /// instead. Both variants produce identical observable output and ordering.
    pub(crate) expansion_seeds: Option<&'a HashSet<PathBuf>>,
}

pub(crate) fn apply_reverse_dependencies_with_provenance<'projects, S: BuildHasher>(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>), S>,
    projects: &[&'projects Project],
    context: ReverseDependencyContext<'_>,
) -> Result<Vec<(PathBuf, String)>> {
    // Fast path: with no project carrying any local
    // (monorepo) dependency edge, the DFS discovers nothing and the
    // full `path_to_name` / `reverse_deps` construction below is pure
    // waste (two hash maps plus a per-project strip_prefix). Common on
    // single-package repos and workspaces that don't use `workspace:*`
    // / `workspace = true` / `[tool.uv.sources]` / `<ProjectReference/>`.
    // `.all(...)` short-circuits on the first dep-carrying project, so
    // it's O(1) amortized when the rest of the function would have
    // fired anyway.
    if projects.iter().all(|p| p.dependencies().is_empty()) {
        return Ok(Vec::new());
    }

    let project_names = ProjectNameAnalysis::new(projects);
    if let Some(ambiguity) = project_names.referenced_ambiguity() {
        return Err(DependencyAmbiguityError::from(ambiguity).into());
    }

    // Ambiguity validation is unconditional. Once it succeeds, an empty map
    // cannot seed reverse-dependency traversal and is safe to return early.
    if update_map.is_empty() {
        return Ok(Vec::new());
    }
    if context.expansion_seeds.is_some_and(HashSet::is_empty) {
        return Ok(Vec::new());
    }

    // Single pass over projects to build:
    //   - path_to_name:   relative file path -> package name (for O(1) reverse lookup)
    //   - reverse_deps:   unique dependency name -> [packages that depend on it]
    //
    // Both maps borrow their paths straight out of `projects`, which outlives this
    // function: `get_relative_path_ref` only strips a prefix, so the result is a
    // subslice of `project.path()` rather than a fresh `PathBuf`. That removes the
    // one owned path per project plus one clone per dependency edge that the owned
    // shape needed before the worklist even started. Lookups are unaffected because
    // `&Path: Borrow<Path>` hashes and compares exactly like `PathBuf`.
    let mut path_to_name: HashMap<&'projects Path, &'projects str> =
        HashMap::with_capacity(projects.len());
    let mut reverse_deps: HashMap<&'projects str, Vec<(&'projects Path, Option<&'projects str>)>> =
        HashMap::with_capacity(projects.len());
    for (idx, project) in projects.iter().enumerate() {
        let rel_path =
            get_relative_path_ref(context.repo_root_path, project.path()).with_context(|| {
                format!(
                    "failed to apply reverse dependencies for project '{}'",
                    project.path().display()
                )
            })?;

        // Borrow the name from `projects`, which outlives both graph maps.
        // Only unique named projects can be initial or transitive worklist
        // seeds. Nameless projects retain the historical "unknown" fallback
        // when reached directly, while duplicate names stop at that direct
        // PATCH and cannot propagate farther.
        let name_opt = project.name();
        let project_name = name_opt.unwrap_or("unknown");
        let worklist_name = match name_opt {
            Some(name) if project_names.resolve(name) == ProjectNameResolution::Unique(idx) => {
                Some(name)
            }
            Some(_) => None,
            None => Some(project_name),
        };

        let dependencies = project.dependencies();
        for dep_name in dependencies {
            if !matches!(
                project_names.resolve(dep_name),
                ProjectNameResolution::Unique(_)
            ) {
                continue;
            }

            // Fast-path: `HashMap::get_mut` on an existing key skips the
            // `entry` API's key move on hits — common when multiple
            // monorepo packages depend on the same core crate (e.g.
            // `bridge/node` + `bridge/python` both depend on
            // `changepacks`). Keys and values are `&str` borrowed from
            // `projects`, so both paths are zero-alloc.
            let entry = if let Some(existing) = reverse_deps.get_mut(dep_name.as_str()) {
                existing
            } else {
                reverse_deps.entry(dep_name.as_str()).or_default()
            };
            entry.push((rel_path, worklist_name));
        }

        // `rel_path` is a `Copy` borrow of `projects`, so both consumers above and
        // below share it without any clone or move dance.
        if let Some(unique_name) = worklist_name.filter(|_| name_opt.is_some()) {
            path_to_name.insert(rel_path, unique_name);
        }
    }

    for dependents in reverse_deps.values_mut() {
        // Unstable sort: the comparator covers both tuple fields and bottoms out
        // in `compare_paths`, which is `Equal` only for byte-identical paths, so
        // an `Equal` pair would have to agree on path bytes AND name — i.e. be
        // observably the same element (downstream only reads the pointed-to
        // content, never the reference identity). Each entry is a distinct
        // project's relative path, so ties do not arise. No scratch allocation.
        dependents.sort_unstable_by(|left, right| {
            compare_paths(left.0, right.0).then_with(|| left.1.cmp(&right.1))
        });
    }

    // Find all packages that need to be updated due to dependencies.
    // `packages_to_add` serves BOTH purposes: the "already scheduled" gate
    // (via `contains_key`) and the final insertion queue (drained into
    // `update_map` below). This collapses the previous parallel
    // `HashSet<PathBuf>` + `Vec<(PathBuf, String)>` structures into a
    // single `HashMap` allocation and removes the per-edge clones entirely
    // (the key is the same borrowed `&Path` the graph already holds, vs. one
    // owned clone for the set insert and another for the vec push).
    let mut packages_to_add: HashMap<&'projects Path, &'projects str> =
        HashMap::with_capacity(projects.len());

    // Seed a breadth-first worklist in path/name order. Keeping every initial
    // update ahead of generated dependents lets independently bumped direct
    // triggers settle the note winner before transitive propagation begins.
    let mut initial_paths = Vec::with_capacity(update_map.len());
    initial_paths.extend(
        update_map
            .keys()
            .filter(|path| {
                context
                    .expansion_seeds
                    .is_none_or(|seeds| seeds.contains(*path))
            })
            .filter_map(|path| {
                path_to_name
                    .get_key_value(path.as_path())
                    .map(|(seed_path, name)| (*seed_path, *name))
            }),
    );
    // Unstable sort: seeds are built from `update_map` keys, so their paths are
    // pairwise distinct, and the comparator covers both tuple fields ending in
    // `compare_paths` (`Equal` only for byte-identical paths). Nothing compares
    // `Equal`, so stability is unobservable and no scratch buffer is allocated.
    initial_paths.sort_unstable_by(|left, right| {
        compare_paths(left.0, right.0).then_with(|| left.1.cmp(right.1))
    });
    // `to_process` is a plain `Vec` used as a FIFO queue: `head` is the read
    // cursor and the tail is the vector's end, so the live queue is exactly the
    // `to_process[head..]` window. Every push — the seed fill below and both
    // pushes inside the loop — is gated on a path being newly reached
    // (`reached_paths.insert` returning `true`, with the seed paths pre-inserted
    // into `reached_paths`), so a given path is enqueued at most once and the
    // total number of pushes is bounded by the number of distinct project paths,
    // i.e. `projects.len()`. The capacity below is therefore an exact upper
    // bound and the vector never reallocates. Because entries are appended in
    // the same order a deque would have received them and consumed in that same
    // order, never revisited, a monotonically advancing head cursor yields
    // byte-identical FIFO order to popping a deque front, while dropping the
    // separate `VecDeque` allocation.
    let mut to_process: Vec<&str> = Vec::with_capacity(projects.len());
    to_process.extend(initial_paths.into_iter().map(|(_, name)| name));
    // `None` seeds every already-scheduled path, so borrow the keys straight out of `update_map`
    // instead of walking a cloned copy of them.
    let mut reached_paths: HashSet<&Path> = match context.expansion_seeds {
        Some(seeds) => {
            let mut reached = HashSet::with_capacity(seeds.len());
            reached.extend(seeds.iter().map(PathBuf::as_path));
            reached
        }
        None => {
            let mut reached = HashSet::with_capacity(update_map.len());
            reached.extend(update_map.keys().map(PathBuf::as_path));
            reached
        }
    };
    let mut head = 0;
    while head < to_process.len() {
        let trigger_name = to_process[head];
        head += 1;
        if let Some(dependents) = reverse_deps.get(trigger_name) {
            for (dependent_path, dependent_name) in dependents {
                let dependent_path = *dependent_path;
                let newly_reached = reached_paths.insert(dependent_path);
                if update_map.contains_key(dependent_path) {
                    if newly_reached && let Some(dependent_name) = dependent_name {
                        to_process.push(*dependent_name);
                    }
                    continue;
                }

                match packages_to_add.entry(dependent_path) {
                    Entry::Vacant(entry) => {
                        entry.insert(trigger_name);
                        if newly_reached && let Some(dependent_name) = dependent_name {
                            to_process.push(*dependent_name);
                        }
                    }
                    Entry::Occupied(mut entry) => {
                        if trigger_name < *entry.get() {
                            entry.insert(trigger_name);
                        }
                    }
                }
            }
        }
    }

    // With no additions, the canonical rebuild is dead work; RandomState iteration is unobservable either way.
    if packages_to_add.is_empty() {
        return Ok(Vec::new());
    }

    // Reinsert every entry in canonical path order. `HashMap` does not promise
    // ordered iteration, but this gives deterministic insertion/serialization
    // whenever the map's chosen hasher supports it.
    // `Drain` is an `ExactSizeIterator`, so `collect` would size the vector to
    // exactly the drained length and leave zero spare capacity — every push
    // below would then start a geometric-doubling realloc chain that memcpys
    // `(PathBuf, (UpdateType, Vec<ChangePackResultLog>))` tuples. Read the len
    // before draining and reserve the final size once instead.
    let existing_len = update_map.len();
    let mut ordered_entries = Vec::with_capacity(existing_len + packages_to_add.len());
    ordered_entries.extend(update_map.drain());
    let mut generated = Vec::with_capacity(packages_to_add.len());
    for (path, dependency_name) in packages_to_add {
        let note =
            format!("Auto-update: depends on '{dependency_name}' via a local workspace dependency");
        // Two owned paths materialize here — one for the caller's `generated`
        // provenance list and one for the `update_map` key — exactly the same
        // count the owned-graph version ended up allocating in this tail.
        generated.push((path.to_path_buf(), note.clone()));
        ordered_entries.push((
            path.to_path_buf(),
            (
                UpdateType::Patch,
                vec![ChangePackResultLog::new(UpdateType::Patch, note)],
            ),
        ));
    }
    // Unstable sort: this comparator looks only at the path, so it needs the keys
    // to be distinct rather than the whole element to be ordered — and they are.
    // The drained half are `HashMap` keys, and every `packages_to_add` path was
    // gated on `!update_map.contains_key(..)` above with no intervening insert,
    // so the two halves are disjoint and internally unique. `compare_paths` is
    // `Equal` only for byte-identical paths, which therefore never happens here,
    // and the unstable sort skips the up-to-n/2 scratch allocation on a vector
    // holding every scheduled update.
    ordered_entries.sort_unstable_by(|left, right| compare_paths(&left.0, &right.0));
    update_map.extend(ordered_entries);

    Ok(generated)
}
#[cfg(test)]
mod tests {
    use std::hash::{BuildHasherDefault, Hasher};

    use changepacks_core::Package;
    use changepacks_node::package::NodePackage;

    use super::*;

    use crate::test_support::create_project;
    #[derive(Default)]
    struct CollisionHasher;

    impl Hasher for CollisionHasher {
        fn finish(&self) -> u64 {
            0
        }

        fn write(&mut self, _bytes: &[u8]) {}
    }

    type DeterministicUpdateMap = HashMap<
        PathBuf,
        (UpdateType, Vec<ChangePackResultLog>),
        BuildHasherDefault<CollisionHasher>,
    >;

    fn insert_reverse_dependency_seed(
        update_map: &mut DeterministicUpdateMap,
        name: &str,
        update_type: UpdateType,
    ) {
        update_map.insert(
            PathBuf::from(format!("{name}/package.json")),
            (
                update_type,
                vec![ChangePackResultLog::new(
                    update_type,
                    format!("Update {name}"),
                )],
            ),
        );
    }

    fn reverse_dependency_order_variant(variant: usize) -> DeterministicUpdateMap {
        let alpha = create_project("alpha", vec![]);
        let zeta = create_project("zeta", vec![]);
        let bridge_a = create_project(
            "bridge-a",
            if matches!(variant, 0 | 2) {
                vec!["zeta", "alpha"]
            } else {
                vec!["alpha", "zeta"]
            },
        );
        let bridge_z = create_project(
            "bridge-z",
            if matches!(variant, 0 | 3) {
                vec!["alpha", "zeta"]
            } else {
                vec!["zeta", "alpha"]
            },
        );
        let app = create_project(
            "app",
            if matches!(variant, 0 | 1) {
                vec!["bridge-z", "bridge-a"]
            } else {
                vec!["bridge-a", "bridge-z"]
            },
        );
        let projects: Vec<&Project> = match variant {
            0 => vec![&zeta, &bridge_z, &alpha, &app, &bridge_a],
            1 => vec![&bridge_a, &alpha, &app, &zeta, &bridge_z],
            2 => vec![&app, &bridge_z, &zeta, &bridge_a, &alpha],
            3 => vec![&alpha, &bridge_a, &zeta, &bridge_z, &app],
            _ => unreachable!("test variant is in range"),
        };

        let mut update_map = HashMap::with_hasher(BuildHasherDefault::default());
        if matches!(variant, 0 | 2) {
            insert_reverse_dependency_seed(&mut update_map, "alpha", UpdateType::Minor);
            insert_reverse_dependency_seed(&mut update_map, "zeta", UpdateType::Major);
        } else {
            insert_reverse_dependency_seed(&mut update_map, "zeta", UpdateType::Major);
            insert_reverse_dependency_seed(&mut update_map, "alpha", UpdateType::Minor);
        }

        apply_reverse_dependencies(&mut update_map, &projects, Path::new("/test")).unwrap();
        update_map
    }

    fn result_note(update_map: &DeterministicUpdateMap, path: &str) -> String {
        serde_json::to_value(&update_map[Path::new(path)].1[0]).unwrap()["note"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn test_apply_reverse_dependencies_empty_update_map_is_noop() {
        let core = create_project("core", vec![]);
        let cli = create_project("cli", vec!["core"]);
        let projects: Vec<&Project> = vec![&core, &cli];
        let mut update_map: HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)> =
            HashMap::new();

        apply_reverse_dependencies(&mut update_map, &projects, Path::new("/test")).unwrap();

        assert!(update_map.is_empty());
    }

    #[test]
    fn test_apply_reverse_dependencies_rejects_project_outside_repo_root() {
        let core = create_project("core", vec![]);
        let mut outside_package = NodePackage::new(
            Some("outside".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/outside/package.json"),
            PathBuf::from("outside/package.json"),
        );
        outside_package.add_dependency("core");
        let outside = Project::Package(Box::new(outside_package));
        let projects: Vec<&Project> = vec![&core, &outside];
        let mut update_map = HashMap::from([(
            PathBuf::from("core/package.json"),
            (UpdateType::Minor, vec![]),
        )]);

        let error = apply_reverse_dependencies(&mut update_map, &projects, Path::new("/test"))
            .expect_err("an out-of-root project must fail reverse dependency traversal");

        assert!(error.to_string().contains("/outside/package.json"));
    }

    #[test]
    fn test_apply_reverse_dependencies_basic() {
        // Setup: core has no deps, cli depends on core
        let core = create_project("core", vec![]);
        let cli = create_project("cli", vec!["core"]);

        let projects: Vec<&Project> = vec![&core, &cli];
        let repo_root = Path::new("/test");

        // Core is being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root).unwrap();

        // cli should be added as PATCH update
        assert_eq!(update_map.len(), 2);
        assert!(update_map.contains_key(&PathBuf::from("cli/package.json")));
        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Patch
        );
        let log_json =
            serde_json::to_value(&update_map[&PathBuf::from("cli/package.json")].1[0]).unwrap();
        assert_eq!(
            log_json["note"],
            "Auto-update: depends on 'core' via a local workspace dependency"
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_transitive() {
        // Setup: core -> utils -> cli (cli depends on utils, utils depends on core)
        let core = create_project("core", vec![]);
        let utils = create_project("utils", vec!["core"]);
        let cli = create_project("cli", vec!["utils"]);

        let projects: Vec<&Project> = vec![&core, &utils, &cli];
        let repo_root = Path::new("/test");

        // Core is being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root).unwrap();

        // Both utils and cli should be added as PATCH updates (transitive)
        assert_eq!(update_map.len(), 3);
        assert!(update_map.contains_key(&PathBuf::from("utils/package.json")));
        assert!(update_map.contains_key(&PathBuf::from("cli/package.json")));
        assert_eq!(
            update_map[&PathBuf::from("utils/package.json")].0,
            UpdateType::Patch
        );
        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Patch
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_no_deps() {
        // Setup: two independent packages
        let core = create_project("core", vec![]);
        let utils = create_project("utils", vec![]);

        let projects: Vec<&Project> = vec![&core, &utils];
        let repo_root = Path::new("/test");

        // Core is being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root).unwrap();

        // utils should NOT be added (no dependency on core)
        assert_eq!(update_map.len(), 1);
        assert!(!update_map.contains_key(&PathBuf::from("utils/package.json")));
    }

    #[test]
    fn test_apply_reverse_dependencies_already_in_update_map() {
        // Setup: cli depends on core, but cli is already in update_map
        let core = create_project("core", vec![]);
        let cli = create_project("cli", vec!["core"]);

        let projects: Vec<&Project> = vec![&core, &cli];
        let repo_root = Path::new("/test");

        // Both core and cli are already being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );
        update_map.insert(
            PathBuf::from("cli/package.json"),
            (
                UpdateType::Major,
                vec![ChangePackResultLog::new(
                    UpdateType::Major,
                    "Update cli".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root).unwrap();

        // cli should remain Major (not overwritten to Patch)
        assert_eq!(update_map.len(), 2);
        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Major
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_diamond() {
        // Diamond pattern: core <- (utils, helpers) <- cli
        // cli depends on both utils and helpers, both depend on core
        let core = create_project("core", vec![]);
        let utils = create_project("utils", vec!["core"]);
        let helpers = create_project("helpers", vec!["core"]);
        let cli = create_project("cli", vec!["utils", "helpers"]);

        let projects: Vec<&Project> = vec![&core, &utils, &helpers, &cli];
        let repo_root = Path::new("/test");

        // Core is being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root).unwrap();

        // All packages should be updated
        assert_eq!(update_map.len(), 4);
        assert!(update_map.contains_key(&PathBuf::from("utils/package.json")));
        assert!(update_map.contains_key(&PathBuf::from("helpers/package.json")));
        assert!(update_map.contains_key(&PathBuf::from("cli/package.json")));
    }

    #[test]
    fn test_apply_reverse_dependencies_missing_dependency() {
        // cli depends on "missing" package that doesn't exist in projects
        let cli = create_project("cli", vec!["missing"]);

        let projects: Vec<&Project> = vec![&cli];
        let repo_root = Path::new("/test");

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("other/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update other".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root).unwrap();

        // No changes, missing dependency is ignored
        assert_eq!(update_map.len(), 1);
    }

    #[test]
    fn test_apply_reverse_dependencies_rejects_referenced_duplicate_names() {
        let mut shared_a = create_project("zeta", vec![]);
        shared_a.set_name("shared".to_string());
        let mut shared_b = create_project("alpha", vec![]);
        shared_b.set_name("shared".to_string());
        let app = create_project("app", vec!["shared"]);
        let projects: Vec<&Project> = vec![&shared_a, &app, &shared_b];
        let repo_root = Path::new("/test");

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("alpha/package.json"),
            (UpdateType::Minor, vec![]),
        );
        let error = apply_reverse_dependencies(&mut update_map, &projects, repo_root)
            .expect_err("a referenced duplicate project name must be ambiguous");

        assert_eq!(
            error.to_string(),
            "ambiguous dependency `shared`: candidates: alpha/package.json, zeta/package.json"
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_empty_map_reports_ambiguity_deterministically() {
        let mut shared_zeta = create_project("zeta", vec![]);
        shared_zeta.set_name("shared".to_string());
        let mut shared_alpha = create_project("alpha", vec![]);
        shared_alpha.set_name("shared".to_string());
        let app = create_project("app", vec!["shared"]);
        let permutations = [
            vec![&shared_zeta, &app, &shared_alpha],
            vec![&shared_alpha, &shared_zeta, &app],
            vec![&app, &shared_alpha, &shared_zeta],
        ];
        let expected =
            "ambiguous dependency `shared`: candidates: alpha/package.json, zeta/package.json";

        let mut messages = Vec::new();
        for projects in permutations {
            let mut update_map = HashMap::new();
            let error = apply_reverse_dependencies(&mut update_map, &projects, Path::new("/test"))
                .expect_err("referenced duplicate names must be ambiguous for an empty map");
            messages.push(error.to_string());
        }

        assert!(messages.iter().all(|message| message == expected));
        assert!(messages.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn test_apply_reverse_dependencies_allows_unreferenced_duplicate_names() {
        let mut shared_a = create_project("zeta", vec![]);
        shared_a.set_name("shared".to_string());
        let mut shared_b = create_project("alpha", vec![]);
        shared_b.set_name("shared".to_string());
        let app = create_project("app", vec![]);
        let projects: Vec<&Project> = vec![&shared_a, &app, &shared_b];
        let mut update_map = HashMap::from([(
            PathBuf::from("alpha/package.json"),
            (UpdateType::Minor, vec![]),
        )]);

        apply_reverse_dependencies(&mut update_map, &projects, Path::new("/test")).unwrap();

        assert_eq!(update_map.len(), 1);
    }

    #[test]
    fn test_apply_reverse_dependencies_unique_name_propagates_directly() {
        let core = create_project("core", vec![]);
        let cli = create_project("cli", vec!["core"]);
        let projects: Vec<&Project> = vec![&core, &cli];
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (UpdateType::Minor, vec![]),
        );

        apply_reverse_dependencies(&mut update_map, &projects, Path::new("/test")).unwrap();

        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Patch
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_unique_names_propagate_transitively() {
        let core = create_project("core", vec![]);
        let utils = create_project("utils", vec!["core"]);
        let cli = create_project("cli", vec!["utils"]);
        let projects: Vec<&Project> = vec![&core, &utils, &cli];
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (UpdateType::Minor, vec![]),
        );

        apply_reverse_dependencies(&mut update_map, &projects, Path::new("/test")).unwrap();

        assert_eq!(
            update_map[&PathBuf::from("utils/package.json")].0,
            UpdateType::Patch
        );
        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Patch
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_chooses_smallest_trigger_deterministically() {
        let expected_alpha_note =
            "Auto-update: depends on 'alpha' via a local workspace dependency";
        let expected_bridge_note =
            "Auto-update: depends on 'bridge-a' via a local workspace dependency";

        for variant in 0..4 {
            let update_map = reverse_dependency_order_variant(variant);

            assert_eq!(
                update_map[Path::new("alpha/package.json")].0,
                UpdateType::Minor,
                "variant {variant}"
            );
            assert_eq!(
                update_map[Path::new("zeta/package.json")].0,
                UpdateType::Major,
                "variant {variant}"
            );
            for path in ["bridge-a/package.json", "bridge-z/package.json"] {
                assert_eq!(
                    update_map[Path::new(path)].0,
                    UpdateType::Patch,
                    "variant {variant}, path {path}"
                );
                assert_eq!(
                    result_note(&update_map, path),
                    expected_alpha_note,
                    "variant {variant}, path {path}"
                );
            }
            assert_eq!(
                update_map[Path::new("app/package.json")].0,
                UpdateType::Patch,
                "variant {variant}"
            );
            assert_eq!(
                result_note(&update_map, "app/package.json"),
                expected_bridge_note,
                "variant {variant}"
            );
        }
    }

    #[test]
    fn test_apply_reverse_dependencies_serializes_equivalent_orders_identically() {
        let serialized: Vec<_> = (0..4)
            .map(|variant| serde_json::to_vec(&reverse_dependency_order_variant(variant)).unwrap())
            .collect();

        assert!(
            serialized.windows(2).all(|pair| pair[0] == pair[1]),
            "equivalent update maps must have byte-identical update types, notes, and ordering"
        );
    }
}
