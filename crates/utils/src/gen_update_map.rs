use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet, VecDeque, hash_map::Entry},
    hash::BuildHasher,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use changepacks_core::{
    ChangePackLog, ChangePackResultLog, Config, Project, UpdateType, normalize_path_separators_of,
};
use glob::Pattern;

use crate::{
    DependencyAmbiguityError, collect_changepack_log_paths, get_relative_path_ref,
    project_names::{ProjectNameAnalysis, ProjectNameResolution, compare_paths},
    read_log_bodies,
};

type UpdateEntry = (UpdateType, Vec<ChangePackResultLog>);
type UpdateMap = HashMap<PathBuf, UpdateEntry>;

/// Reserved prefix for generated carry-forward changepack logs.
pub const CARRY_FORWARD_LOG_PREFIX: &str = "changepack_log_carry_forward_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeneratedState {
    Fresh,
    Persisted,
}

#[derive(Debug)]
enum UpdateProvenance {
    Explicit,
    Generated {
        notes: Vec<String>,
        state: GeneratedState,
    },
}

/// Planned updates together with their explicit or generated origin.
#[derive(Debug)]
pub struct UpdatePlan {
    updates: UpdateMap,
    provenance: HashMap<PathBuf, UpdateProvenance>,
    expansion_seeds: HashSet<PathBuf>,
}

impl UpdatePlan {
    fn record_generated(&mut self, generated: impl IntoIterator<Item = (PathBuf, String)>) {
        for (path, note) in generated {
            self.provenance.insert(
                path,
                UpdateProvenance::Generated {
                    notes: vec![note],
                    state: GeneratedState::Fresh,
                },
            );
        }
    }

    /// Apply reverse-dependency expansion while retaining generated provenance.
    ///
    /// # Errors
    /// Returns an error when project names are ambiguous or a project path is outside the repo.
    pub fn apply_reverse_dependencies(
        &mut self,
        projects: &[&Project],
        repo_root_path: &Path,
    ) -> Result<()> {
        let generated = apply_reverse_dependencies_with_provenance(
            &mut self.updates,
            projects,
            ReverseDependencyContext {
                repo_root_path,
                expansion_seeds: Some(&self.expansion_seeds),
            },
        )?;
        self.record_generated(generated);
        Ok(())
    }

    /// Fold source provenance into the workspace paths that own their bumps.
    pub fn merge_provenance(&mut self, merged_pairs: &[(PathBuf, PathBuf)]) {
        for (source_path, target_path) in merged_pairs {
            if self.expansion_seeds.remove(source_path) {
                self.expansion_seeds.insert(target_path.clone());
            }
            let Some(source) = self.provenance.remove(source_path) else {
                continue;
            };
            match self.provenance.entry(target_path.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(source);
                }
                Entry::Occupied(mut entry) => match (entry.get_mut(), source) {
                    (
                        UpdateProvenance::Explicit,
                        UpdateProvenance::Explicit | UpdateProvenance::Generated { .. },
                    ) => {}
                    (target @ UpdateProvenance::Generated { .. }, UpdateProvenance::Explicit) => {
                        *target = UpdateProvenance::Explicit;
                    }
                    (
                        UpdateProvenance::Generated {
                            notes: target_notes,
                            state: target_state,
                        },
                        UpdateProvenance::Generated {
                            notes: mut source_notes,
                            state: source_state,
                        },
                    ) => {
                        target_notes.append(&mut source_notes);
                        if source_state == GeneratedState::Persisted {
                            *target_state = GeneratedState::Persisted;
                        }
                    }
                },
            }
        }
    }

    /// Retain selected updates and return changepack logs for excluded generated updates.
    pub fn retain_updates(&mut self, mut retain: impl FnMut(&Path) -> bool) -> Vec<ChangePackLog> {
        let mut excluded_paths = self
            .updates
            .keys()
            .filter(|path| !retain(path))
            .cloned()
            .collect::<Vec<_>>();
        excluded_paths.sort_by(|left, right| compare_paths(left, right));

        let mut carry_forward = Vec::new();
        for path in excluded_paths {
            let Some((update_type, _)) = self.updates.remove(&path) else {
                continue;
            };
            if let Some(UpdateProvenance::Generated {
                notes,
                state: GeneratedState::Fresh,
            }) = self.provenance.remove(&path)
            {
                carry_forward.extend(notes.into_iter().map(|note| {
                    ChangePackLog::new(BTreeMap::from([(path.clone(), update_type)]), note)
                }));
            }
        }
        carry_forward
    }
}

impl Deref for UpdatePlan {
    type Target = UpdateMap;

    fn deref(&self) -> &Self::Target {
        &self.updates
    }
}

impl DerefMut for UpdatePlan {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.updates
    }
}

/// Generate update map from changepack logs
///
/// # Errors
/// Returns error if reading changepacks, parsing JSON, or validating `updateOn` rules fails.
pub async fn gen_update_map(changepacks_dir: &Path, config: &Config) -> Result<UpdatePlan> {
    // Two-phase reader (mirrors `clear_update_logs`):
    //   Phase 1: single directory walk to collect the paths of every matching
    //            `changepack_log_*.json` entry — pure name filtering, no IO body.
    //   Phase 2: the shared `read_log_bodies` helper reads every body
    //            concurrently via `try_join_all`, collapsing N sequential
    //            `read_to_string` round-trips into one parallel batch on
    //            IO-bound systems.
    //   Phase 3: the existing sequential parse+merge loop is unchanged. Final
    //            output ordering was never derived from filesystem order — the
    //            `ret.0 > *update_type` guard is driven by `UpdateType::Ord` and
    //            already collapses duplicates across files — so parallelizing
    //            the reads is deterministic and cannot change the observable
    //            update_map for any input.
    let paths = collect_changepack_log_paths(changepacks_dir).await?;
    // Preallocate against `paths.len()` — a tight lower bound because each
    // changepack log's `changes` map usually names one or more distinct
    // project paths. Matches the preallocation policy already applied in
    // `sort_by_dep.rs`, `find_project_dirs.rs`, and
    // `apply_reverse_dependencies`. Eliminates the first geometric-doubling
    // reallocation on any repo with more than one changepack log; byte-
    // identical map contents.
    let mut update_map = HashMap::with_capacity(paths.len());
    let mut provenance = HashMap::with_capacity(paths.len());
    let mut expansion_seeds = HashSet::with_capacity(paths.len());
    let bodies = read_log_bodies(&paths, "changepack log").await?;
    // Zip `paths` with `bodies` so a malformed `changepack_log_*.json`
    // surfaces WHICH file failed rather than a bare `serde_json` error.
    // Users then jump straight to the offender instead of grepping every
    // changepack log. Matches the `with_context` pattern already applied
    // in `get_changepacks_config.rs` and `find_project_dirs.rs`, and
    // costs zero on the happy path (the closure is only invoked on the
    // error path). `try_join_all` above already guarantees `bodies.len()
    // == paths.len()` so the zip is in-lockstep.
    for (path, body) in paths.iter().zip(bodies) {
        let file_json: ChangePackLog = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse changepack log {}", path.display()))?;
        let is_carry_forward = path.file_name().is_some_and(|file_name| {
            file_name
                .to_string_lossy()
                .starts_with(CARRY_FORWARD_LOG_PREFIX)
        });
        for (project_path, update_type) in file_json.changes() {
            if is_carry_forward {
                match provenance.entry(project_path.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(UpdateProvenance::Generated {
                            notes: vec![file_json.note().to_string()],
                            state: GeneratedState::Persisted,
                        });
                    }
                    Entry::Occupied(mut entry) => match entry.get_mut() {
                        UpdateProvenance::Explicit => {}
                        UpdateProvenance::Generated { notes, state } => {
                            notes.push(file_json.note().to_string());
                            *state = GeneratedState::Persisted;
                        }
                    },
                }
            } else {
                // Probe by borrow before allocating, exactly as the `update_map`
                // fast-path below and `apply_update_on_rules_from` already do.
                // Overwrite semantics are preserved: an explicit log still
                // REPLACES an existing `Generated` provenance, the write just
                // reuses the key already stored in the map instead of cloning a
                // second `PathBuf` the insert would immediately drop.
                if let Some(slot) = provenance.get_mut(project_path) {
                    *slot = UpdateProvenance::Explicit;
                } else {
                    provenance.insert(project_path.clone(), UpdateProvenance::Explicit);
                }
                if !expansion_seeds.contains(project_path.as_path()) {
                    expansion_seeds.insert(project_path.clone());
                }
            }
            // Fast-path: `HashMap::get_mut` on an existing key is zero-alloc,
            // whereas `entry(project_path.clone()).or_insert(...)` unconditionally
            // clones the `PathBuf` even when the entry already exists. On repos
            // with many changepack logs mentioning the same package paths (common
            // in active monorepos), this saves N `PathBuf` allocations per hot
            // invocation. Semantics are byte-identical: both branches produce the
            // same mutable reference for the same input. The same probe-before-
            // allocate policy now covers all three containers built by this loop:
            // `update_map`, `provenance`, and `expansion_seeds`.
            let ret = if let Some(existing) = update_map.get_mut(project_path) {
                existing
            } else {
                update_map
                    .entry(project_path.clone())
                    .or_insert((*update_type, vec![]))
            };
            ret.1.push(ChangePackResultLog::new(
                *update_type,
                file_json.note().to_string(),
            ));
            if ret.0 > *update_type {
                ret.0 = *update_type;
            }
        }
    }

    // Apply updateOn rules: if any updated package matches a trigger pattern,
    // add dependent packages as PATCH updates
    let generated = apply_update_on_rules_from(&mut update_map, config, &mut expansion_seeds)?;

    let mut plan = UpdatePlan {
        updates: update_map,
        provenance,
        expansion_seeds,
    };
    plan.record_generated(generated);
    Ok(plan)
}

fn apply_update_on_rules_from(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    config: &Config,
    expansion_seeds: &mut HashSet<PathBuf>,
) -> Result<Vec<(PathBuf, String)>> {
    if config.update_on.is_empty() {
        return Ok(Vec::new());
    }

    // Compile every pattern once and validate configuration even when there
    // are no pending updates.
    let rules: Vec<(&str, Pattern, &[String])> = config
        .update_on
        .iter()
        .map(|(trigger_pattern, dependents)| {
            let pattern = Pattern::new(trigger_pattern).with_context(|| {
                format!("invalid glob pattern in updateOn config: {trigger_pattern}")
            })?;
            Ok((trigger_pattern.as_str(), pattern, dependents.as_slice()))
        })
        .collect::<Result<_>>()?;

    if update_map.is_empty() || expansion_seeds.is_empty() {
        return Ok(Vec::new());
    }

    let mut generated = Vec::new();
    // Double-buffered breadth-first frontier. `current` holds the level being
    // expanded, `next` accumulates the level discovered while expanding it, and
    // the two are swapped at the end of each level. This replaces a
    // `VecDeque` frontier that had to be fully drained into a freshly allocated
    // `Vec` on every level purely so the level could be sorted in place and
    // borrowed as a contiguous slice: with two `Vec`s the storage is reused
    // across levels, so after the first couple of levels the loop performs no
    // frontier allocation at all. Ordering is unchanged — `VecDeque::drain(..)`
    // yielded front-to-back, which is exactly `Vec` iteration order, and the
    // per-level sort below re-imposes canonical order regardless.
    let mut current: Vec<PathBuf> = expansion_seeds.iter().cloned().collect();
    let mut next: Vec<PathBuf> = Vec::new();

    while !current.is_empty() {
        // Preserve BTreeMap rule precedence within each breadth-first batch so
        // the lexicographically first matching trigger owns an inserted note.
        // The per-batch sort establishes canonical path order for every expansion level.
        current.sort_by(|left, right| compare_paths(left, right));
        // Glob triggers in `updateOn` are written with forward slashes, so the
        // filesystem-derived path is normalized through the shared core helper,
        // which keeps the allocation policy in one place. Normalization happens
        // AFTER the sort so it never perturbs batch ordering, and the result is
        // kept as the helper's `Cow`: it borrows straight out of the owning
        // `PathBuf` for every valid-UTF-8 path without a backslash — every Unix
        // path and every already-forward-slashed config path — so the common
        // case allocates nothing, and only a genuine backslash path pays for an
        // owned `String`. `Pattern::matches` takes a `&str`, which `&Cow<str>`
        // derefs to exactly as the previously owned `String` did.
        let match_paths: Vec<Cow<'_, str>> = current
            .iter()
            .map(|path| normalize_path_separators_of(path))
            .collect();

        // `expansion_seeds` records every queued path, including a persisted
        // generated entry reached by a fresh explicit path, so each path reaches
        // every rule exactly once within this plan.
        for (trigger_pattern, pattern, dependents) in &rules {
            for match_path in &match_paths {
                if !pattern.matches(match_path) {
                    continue;
                }

                for dependent in *dependents {
                    // Probe by borrow before allocating: `HashMap::insert` and
                    // `HashSet::insert` take their key by value, so reaching them at
                    // all needs an owned `PathBuf` even when the key is already
                    // present -- the steady state here, because this breadth-first
                    // expansion revisits the same dependents across batches.
                    // `contains_key` / `contains` accept a `&Path` through `Borrow`
                    // (and `Path` hashes identically to `PathBuf`), so a doubly-hit
                    // dependent allocates nothing. Each surviving consumer below then
                    // builds exactly the one owned `PathBuf` it stores: `PathBuf::from`
                    // of a `&Path` costs what cloning a shared binding costs, so no
                    // case regresses, and an update-only hit no longer allocates a
                    // spare binding that is dropped unused.
                    let dependent_ref = Path::new(dependent.as_str());
                    let needs_update = !update_map.contains_key(dependent_ref);
                    let needs_seed = !expansion_seeds.contains(dependent_ref);
                    if !needs_update && !needs_seed {
                        continue;
                    }

                    if needs_update {
                        let note =
                            format!("Auto-update triggered by updateOn rule: {trigger_pattern}");
                        update_map.insert(
                            PathBuf::from(dependent_ref),
                            (
                                UpdateType::Patch,
                                vec![ChangePackResultLog::new(UpdateType::Patch, note.clone())],
                            ),
                        );
                        generated.push((PathBuf::from(dependent_ref), note));
                    }
                    if needs_seed {
                        expansion_seeds.insert(PathBuf::from(dependent_ref));
                        next.push(PathBuf::from(dependent_ref));
                    }
                }
            }
        }

        // Drop the borrow of `current` held by `match_paths` before swapping.
        drop(match_paths);
        std::mem::swap(&mut current, &mut next);
        next.clear();
    }

    Ok(generated)
}

/// Apply reverse dependency updates: if package A depends on package B (via a local workspace dependency),
/// and B is being updated, then A should also be updated as PATCH.
///
/// Every already-scheduled path in `update_map` is treated as an expansion seed. Callers holding an
/// [`UpdatePlan`] must use [`UpdatePlan::apply_reverse_dependencies`] instead, so that generated
/// provenance and the narrower seed set are preserved.
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

struct ReverseDependencyContext<'a> {
    repo_root_path: &'a Path,
    /// Paths allowed to seed reverse-dependency expansion.
    ///
    /// `None` means "every key of `update_map` is a seed", which is exactly what the free
    /// [`apply_reverse_dependencies`] wants. Spelling that as `Some(update_map.keys().cloned())`
    /// would allocate a whole `HashSet` and clone every `PathBuf` only to compare the map's keys
    /// against a byte-for-byte copy of themselves, so the unrestricted case is encoded in the type
    /// instead. Both variants produce identical observable output and ordering.
    expansion_seeds: Option<&'a HashSet<PathBuf>>,
}

fn apply_reverse_dependencies_with_provenance<'projects, S: BuildHasher>(
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
        dependents.sort_by(|left, right| {
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
    initial_paths
        .sort_by(|left, right| compare_paths(left.0, right.0).then_with(|| left.1.cmp(right.1)));
    let mut to_process: VecDeque<&str> = initial_paths.into_iter().map(|(_, name)| name).collect();
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
    while let Some(trigger_name) = to_process.pop_front() {
        if let Some(dependents) = reverse_deps.get(trigger_name) {
            for (dependent_path, dependent_name) in dependents {
                let dependent_path = *dependent_path;
                let newly_reached = reached_paths.insert(dependent_path);
                if update_map.contains_key(dependent_path) {
                    if newly_reached && let Some(dependent_name) = dependent_name {
                        to_process.push_back(*dependent_name);
                    }
                    continue;
                }

                match packages_to_add.entry(dependent_path) {
                    Entry::Vacant(entry) => {
                        entry.insert(trigger_name);
                        if newly_reached && let Some(dependent_name) = dependent_name {
                            to_process.push_back(*dependent_name);
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
    ordered_entries.sort_by(|left, right| compare_paths(&left.0, &right.0));
    update_map.extend(ordered_entries);

    Ok(generated)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap},
        hash::{BuildHasherDefault, Hasher},
    };

    use changepacks_core::{Config, Package};
    use changepacks_node::package::NodePackage;
    use tempfile::TempDir;
    use tokio::fs;

    use super::*;

    use crate::test_support::create_project;

    /// Test shim over `apply_update_on_rules_from` that treats every
    /// `update_map` key as an expansion seed.
    fn apply_update_on_rules(
        update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
        config: &Config,
    ) -> Result<Vec<(PathBuf, String)>> {
        let mut expansion_seeds = update_map.keys().cloned().collect();
        apply_update_on_rules_from(update_map, config, &mut expansion_seeds)
    }

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

    /// Create a `.changepacks` directory inside `temp_dir` and return its path.
    async fn provenance_fixture_dir(temp_dir: &TempDir) -> PathBuf {
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();
        changepacks_dir
    }

    /// Write one changepack log naming a single project path.
    ///
    /// `file_name` decides which provenance `gen_update_map` records: a name
    /// starting with [`CARRY_FORWARD_LOG_PREFIX`] produces
    /// `Generated { state: Persisted }`, any other `changepack_log_*.json`
    /// produces `Explicit` and seeds expansion. `Generated { state: Fresh }` has
    /// no on-disk representation at all — it only ever arises from an `updateOn`
    /// rule or reverse-dependency expansion, so the tests below mint it through
    /// `Config::update_on`.
    async fn write_provenance_log(
        changepacks_dir: &Path,
        file_name: &str,
        project_path: &str,
        update_type: UpdateType,
        note: &str,
    ) {
        let log = ChangePackLog::new(
            BTreeMap::from([(PathBuf::from(project_path), update_type)]),
            note.to_string(),
        );
        fs::write(
            changepacks_dir.join(file_name),
            serde_json::to_vec(&log).unwrap(),
        )
        .await
        .unwrap();
    }

    fn update_on_note(trigger: &str) -> String {
        format!("Auto-update triggered by updateOn rule: {trigger}")
    }

    /// Drop `excluded` from `plan` and project the resulting carry-forward logs
    /// down to `(project path, note)` pairs.
    ///
    /// [`UpdatePlan::retain_updates`] is the only public observer of provenance:
    /// an excluded path emits one log per recorded note when — and only when —
    /// its provenance is `Generated { state: Fresh }`. `Explicit`,
    /// `Generated { state: Persisted }`, and absent provenance all emit nothing.
    /// So the returned vector distinguishes every state `merge_provenance` can
    /// leave a path in, and its order mirrors the stored note order.
    fn carry_forward_for(plan: &mut UpdatePlan, excluded: &[&str]) -> Vec<(PathBuf, String)> {
        plan.retain_updates(|path| {
            !excluded
                .iter()
                .any(|candidate| path == Path::new(candidate))
        })
        .iter()
        .map(|log| {
            let (path, _) = log
                .changes()
                .iter()
                .next()
                .expect("a carry-forward log names exactly one project path");
            (path.clone(), log.note().to_string())
        })
        .collect()
    }

    #[tokio::test]
    async fn test_gen_update_map() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        let config = Config::default();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);
        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();

        {
            assert!(
                gen_update_map(&changepacks_dir, &config)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        {
            fs::write(
                changepacks_dir.join("config.json"),
                serde_json::to_string(&Config::default()).unwrap(),
            )
            .await
            .unwrap();
            assert!(
                gen_update_map(&changepacks_dir, &config)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        {
            fs::write(changepacks_dir.join("wrong.file"), "{}")
                .await
                .unwrap();
            assert!(
                gen_update_map(&changepacks_dir, &config)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        {
            let mut map = BTreeMap::new();
            map.insert(temp_path.join("package"), UpdateType::Patch);
            let changepack_log = ChangePackLog::new(map, "".to_string());

            fs::write(
                changepacks_dir.join("changepack_log_1.json"),
                serde_json::to_string(&changepack_log).unwrap(),
            )
            .await
            .unwrap();
            let update_map = gen_update_map(&changepacks_dir, &config).await.unwrap();
            assert!(update_map.len() == 1);
            assert!(update_map.contains_key(&temp_path.join("package")));
            assert!(update_map[&temp_path.join("package")].0 == UpdateType::Patch);
        }

        {
            let update_map = gen_update_map(&changepacks_dir, &config).await.unwrap();
            assert!(update_map.len() == 1);

            let mut map = BTreeMap::new();
            map.insert(temp_path.join("package"), UpdateType::Minor);
            let changepack_log = ChangePackLog::new(map, "".to_string());

            fs::write(
                changepacks_dir.join("changepack_log_2.json"),
                serde_json::to_string(&changepack_log).unwrap(),
            )
            .await
            .unwrap();
            let update_map = gen_update_map(&changepacks_dir, &config).await.unwrap();
            assert!(update_map.len() == 1);
            assert!(update_map.contains_key(&temp_path.join("package")));
            // overwrite the previous update type
            assert!(update_map[&temp_path.join("package")].0 == UpdateType::Minor);
        }
        {
            let mut map = BTreeMap::new();
            map.insert(temp_path.join("package2"), UpdateType::Major);
            let changepack_log = ChangePackLog::new(map, "".to_string());

            fs::write(
                changepacks_dir.join("changepack_log_3.json"),
                serde_json::to_string(&changepack_log).unwrap(),
            )
            .await
            .unwrap();
            let update_map = gen_update_map(&changepacks_dir, &config).await.unwrap();
            assert!(update_map.len() == 2);
            assert!(update_map.contains_key(&temp_path.join("package2")));
            assert!(update_map[&temp_path.join("package2")].0 == UpdateType::Major);
        }
        {
            let mut map = BTreeMap::new();
            map.insert(temp_path.join("package2"), UpdateType::Patch);
            let changepack_log = ChangePackLog::new(map, "".to_string());

            fs::write(
                changepacks_dir.join("changepack_log_4.json"),
                serde_json::to_string(&changepack_log).unwrap(),
            )
            .await
            .unwrap();
            let update_map = gen_update_map(&changepacks_dir, &config).await.unwrap();
            assert!(update_map.len() == 2);
            assert!(update_map.contains_key(&temp_path.join("package2")));
            // remain
            assert!(update_map[&temp_path.join("package2")].0 == UpdateType::Major);
        }
        temp_dir.close().unwrap();
    }

    // Regression: a malformed `changepack_log_*.json` must surface WHICH
    // file failed to parse rather than a bare `serde_json` error, so users
    // can jump straight to the offender instead of grepping every
    // changepack log. Locks in the `.with_context(|| format!("Failed to
    // parse changepack log {}", path.display()))` wrapper on the
    // `serde_json::from_str` call above.
    #[tokio::test]
    async fn test_gen_update_map_reports_bad_log_path() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        let config = Config::default();

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();

        // Drop a malformed log into the changepacks dir. `not valid json`
        // fails the `serde_json::from_str` step deterministically.
        let bad_log_path = changepacks_dir.join("changepack_log_bad.json");
        fs::write(&bad_log_path, "not valid json").await.unwrap();

        let err = gen_update_map(&changepacks_dir, &config)
            .await
            .expect_err("malformed changepack log must surface as an error");
        // `{err:#}` renders the anyhow chain, so the wrapping
        // `with_context` message is included alongside the inner
        // `serde_json` error.
        let msg = format!("{err:#}");
        assert!(
            msg.contains("changepack_log_bad.json"),
            "expected path in error message, got: {msg}"
        );
        assert!(
            msg.contains("Failed to parse changepack log"),
            "expected context prefix in error message, got: {msg}"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gen_update_map_reads_only_named_changepack_logs() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();

        let valid_path = temp_path.join("packages/valid");
        let ignored_path = temp_path.join("packages/notes");
        let valid_log = ChangePackLog::new(
            BTreeMap::from([(valid_path.clone(), UpdateType::Minor)]),
            "valid".to_string(),
        );
        let notes = ChangePackLog::new(
            BTreeMap::from([(ignored_path.clone(), UpdateType::Major)]),
            "user notes".to_string(),
        );
        fs::write(
            changepacks_dir.join("changepack_log_valid.JSON"),
            serde_json::to_vec(&valid_log).unwrap(),
        )
        .await
        .unwrap();
        fs::write(
            changepacks_dir.join("notes.json"),
            serde_json::to_vec(&notes).unwrap(),
        )
        .await
        .unwrap();

        let update_map = gen_update_map(&changepacks_dir, &Config::default())
            .await
            .unwrap();

        assert_eq!(update_map.len(), 1);
        assert_eq!(update_map[&valid_path].0, UpdateType::Minor);
        assert!(!update_map.contains_key(&ignored_path));
    }

    #[tokio::test]
    async fn test_gen_update_map_ignores_json_directory() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        let config = Config::default();

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();

        let json_dir_path = changepacks_dir.join("changepack_log_directory.json");
        fs::create_dir(&json_dir_path).await.unwrap();

        let update_map = gen_update_map(&changepacks_dir, &config)
            .await
            .expect("JSON-named directory must be ignored");
        assert!(update_map.is_empty());
        assert!(
            json_dir_path.is_dir(),
            "JSON-named directory must be preserved"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_on_rules() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();

        // Create config with updateOn rule
        let mut update_on = BTreeMap::new();
        update_on.insert(
            "crates/*".to_string(),
            vec!["bridge/node".to_string(), "bridge/python".to_string()],
        );
        let config = Config {
            update_on,
            ..Default::default()
        };

        // Create a changepack log for crates/core
        let mut map = BTreeMap::new();
        map.insert(PathBuf::from("crates/core"), UpdateType::Minor);
        let changepack_log = ChangePackLog::new(map, "Update core".to_string());

        fs::write(
            changepacks_dir.join("changepack_log_update_on.json"),
            serde_json::to_string(&changepack_log).unwrap(),
        )
        .await
        .unwrap();

        let update_map = gen_update_map(&changepacks_dir, &config).await.unwrap();

        // Should have 3 entries: crates/core (Minor), bridge/node (Patch), bridge/python (Patch)
        assert_eq!(update_map.len(), 3);
        assert!(update_map.contains_key(&PathBuf::from("crates/core")));
        assert!(update_map.contains_key(&PathBuf::from("bridge/node")));
        assert!(update_map.contains_key(&PathBuf::from("bridge/python")));

        // Original update should remain Minor
        assert_eq!(
            update_map[&PathBuf::from("crates/core")].0,
            UpdateType::Minor
        );
        // Dependent updates should be Patch
        assert_eq!(
            update_map[&PathBuf::from("bridge/node")].0,
            UpdateType::Patch
        );
        assert_eq!(
            update_map[&PathBuf::from("bridge/python")].0,
            UpdateType::Patch
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn persisted_carry_plan_does_not_seed_update_on_or_reverse_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir(&changepacks_dir).await.unwrap();
        let carry = ChangePackLog::new(
            BTreeMap::from([(PathBuf::from("core/package.json"), UpdateType::Patch)]),
            "persisted generated core".to_string(),
        );
        fs::write(
            changepacks_dir.join(format!("{CARRY_FORWARD_LOG_PREFIX}test.json")),
            serde_json::to_vec(&carry).unwrap(),
        )
        .await
        .unwrap();
        let config = Config {
            update_on: BTreeMap::from([(
                "core/package.json".to_string(),
                vec!["update-on/package.json".to_string()],
            )]),
            ..Default::default()
        };
        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();
        let core = create_project("core", vec![]);
        let cli = create_project("cli", vec!["core"]);

        plan.apply_reverse_dependencies(&[&core, &cli], Path::new("/test"))
            .unwrap();

        assert_eq!(plan.len(), 1);
        assert!(plan.contains_key(Path::new("core/package.json")));
        assert!(!plan.contains_key(Path::new("update-on/package.json")));
        assert!(!plan.contains_key(Path::new("cli/package.json")));
    }

    #[tokio::test]
    async fn explicit_log_dominates_persisted_carry_for_fresh_expansion() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir(&changepacks_dir).await.unwrap();
        let path = PathBuf::from("core/package.json");
        let carry = ChangePackLog::new(
            BTreeMap::from([(path.clone(), UpdateType::Patch)]),
            "persisted generated core".to_string(),
        );
        let explicit = ChangePackLog::new(
            BTreeMap::from([(path.clone(), UpdateType::Minor)]),
            "fresh explicit core".to_string(),
        );
        fs::write(
            changepacks_dir.join(format!("{CARRY_FORWARD_LOG_PREFIX}test.json")),
            serde_json::to_vec(&carry).unwrap(),
        )
        .await
        .unwrap();
        fs::write(
            changepacks_dir.join("changepack_log_explicit.json"),
            serde_json::to_vec(&explicit).unwrap(),
        )
        .await
        .unwrap();
        let generated_path = PathBuf::from("bridge/node/package.json");
        let config = Config {
            update_on: BTreeMap::from([(
                "core/package.json".to_string(),
                vec![generated_path.to_string_lossy().into_owned()],
            )]),
            ..Default::default()
        };

        let plan = gen_update_map(&changepacks_dir, &config).await.unwrap();

        assert_eq!(plan[&path].0, UpdateType::Minor);
        assert_eq!(plan[&generated_path].0, UpdateType::Patch);
    }

    // `merge_provenance` skips any pair whose SOURCE has no recorded
    // provenance (the `let ... else { continue }` guard). The target must be
    // left byte-for-byte as it was — still generated, still fresh, still
    // holding its single note.
    #[tokio::test]
    async fn merge_provenance_skips_source_without_provenance() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_seed.json",
            "seed/package.json",
            UpdateType::Minor,
            "explicit seed",
        )
        .await;
        let config = Config {
            update_on: BTreeMap::from([(
                "seed/package.json".to_string(),
                vec!["target/package.json".to_string()],
            )]),
            ..Default::default()
        };
        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();

        // `ghost/package.json` appears in no log and no rule, so it has no
        // provenance entry to fold anywhere.
        plan.merge_provenance(&[(
            PathBuf::from("ghost/package.json"),
            PathBuf::from("target/package.json"),
        )]);

        assert_eq!(
            carry_forward_for(&mut plan, &["target/package.json"]),
            vec![(
                PathBuf::from("target/package.json"),
                update_on_note("seed/package.json")
            )]
        );
    }

    // A target with no provenance of its own adopts the source's verbatim.
    // This is the shape `changepacks update` actually produces:
    // `merge_workspace_inherited_updates` writes the workspace root straight
    // into the update map (through `DerefMut`), so the root reaches
    // `merge_provenance` with a vacant provenance slot.
    #[tokio::test]
    async fn merge_provenance_vacant_target_adopts_source() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_seed.json",
            "seed/package.json",
            UpdateType::Minor,
            "explicit seed",
        )
        .await;
        let config = Config {
            update_on: BTreeMap::from([(
                "seed/package.json".to_string(),
                vec!["member/Cargo.toml".to_string()],
            )]),
            ..Default::default()
        };
        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();
        plan.insert(
            PathBuf::from("workspace/Cargo.toml"),
            (UpdateType::Patch, vec![]),
        );

        plan.merge_provenance(&[(
            PathBuf::from("member/Cargo.toml"),
            PathBuf::from("workspace/Cargo.toml"),
        )]);

        // Only the workspace root carries forward: the member's provenance was
        // moved out, so excluding it emits nothing.
        assert_eq!(
            carry_forward_for(&mut plan, &["member/Cargo.toml", "workspace/Cargo.toml"]),
            vec![(
                PathBuf::from("workspace/Cargo.toml"),
                update_on_note("seed/package.json")
            )]
        );
    }

    // An `Explicit` target outranks a `Generated` source and is left untouched:
    // a user-authored bump must never be downgraded into a carry-forward
    // candidate by a folded member.
    #[tokio::test]
    async fn merge_provenance_keeps_explicit_target_over_generated_source() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_seed.json",
            "seed/package.json",
            UpdateType::Minor,
            "explicit seed",
        )
        .await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_target.json",
            "target/package.json",
            UpdateType::Minor,
            "explicit target",
        )
        .await;
        let config = Config {
            update_on: BTreeMap::from([(
                "seed/package.json".to_string(),
                vec!["source/package.json".to_string()],
            )]),
            ..Default::default()
        };

        // Control: without the merge the source alone would carry forward, so
        // the empty result below is the merge's doing and not an inert fixture.
        let mut control = gen_update_map(&changepacks_dir, &config).await.unwrap();
        assert_eq!(
            carry_forward_for(
                &mut control,
                &["source/package.json", "target/package.json"]
            ),
            vec![(
                PathBuf::from("source/package.json"),
                update_on_note("seed/package.json")
            )]
        );

        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();
        plan.merge_provenance(&[(
            PathBuf::from("source/package.json"),
            PathBuf::from("target/package.json"),
        )]);

        assert!(
            carry_forward_for(&mut plan, &["source/package.json", "target/package.json"])
                .is_empty()
        );
    }

    // The mirror image: an `Explicit` source PROMOTES a `Generated` target, so
    // the folded bump stops being a carry-forward candidate.
    #[tokio::test]
    async fn merge_provenance_explicit_source_promotes_generated_target() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_seed.json",
            "seed/package.json",
            UpdateType::Minor,
            "explicit seed",
        )
        .await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_source.json",
            "source/package.json",
            UpdateType::Minor,
            "explicit source",
        )
        .await;
        let config = Config {
            update_on: BTreeMap::from([(
                "seed/package.json".to_string(),
                vec!["target/package.json".to_string()],
            )]),
            ..Default::default()
        };

        // Control: the un-merged target is generated + fresh, so it carries forward.
        let mut control = gen_update_map(&changepacks_dir, &config).await.unwrap();
        assert_eq!(
            carry_forward_for(&mut control, &["target/package.json"]),
            vec![(
                PathBuf::from("target/package.json"),
                update_on_note("seed/package.json")
            )]
        );

        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();
        plan.merge_provenance(&[(
            PathBuf::from("source/package.json"),
            PathBuf::from("target/package.json"),
        )]);

        assert!(carry_forward_for(&mut plan, &["target/package.json"]).is_empty());
    }

    // Two `Generated` entries concatenate their notes, target's first, and stay
    // fresh when neither side is persisted — so the excluded target replays
    // BOTH auto-update reasons on the next run.
    #[tokio::test]
    async fn merge_provenance_appends_generated_notes_in_target_order() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_a.json",
            "a/package.json",
            UpdateType::Minor,
            "explicit a",
        )
        .await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_b.json",
            "b/package.json",
            UpdateType::Minor,
            "explicit b",
        )
        .await;
        // Distinct triggers give the two generated entries distinguishable notes.
        let config = Config {
            update_on: BTreeMap::from([
                (
                    "a/package.json".to_string(),
                    vec!["target/package.json".to_string()],
                ),
                (
                    "b/package.json".to_string(),
                    vec!["source/package.json".to_string()],
                ),
            ]),
            ..Default::default()
        };
        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();

        plan.merge_provenance(&[(
            PathBuf::from("source/package.json"),
            PathBuf::from("target/package.json"),
        )]);

        assert_eq!(
            carry_forward_for(&mut plan, &["source/package.json", "target/package.json"]),
            vec![
                (
                    PathBuf::from("target/package.json"),
                    update_on_note("a/package.json")
                ),
                (
                    PathBuf::from("target/package.json"),
                    update_on_note("b/package.json")
                ),
            ]
        );
    }

    // A persisted `Generated` source promotes a fresh `Generated` target to
    // persisted, which suppresses carry-forward entirely: the notes are already
    // on disk in a `changepack_log_carry_forward_*.json`, so re-emitting them
    // would duplicate the log.
    #[tokio::test]
    async fn merge_provenance_persisted_source_promotes_fresh_target() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_seed.json",
            "seed/package.json",
            UpdateType::Minor,
            "explicit seed",
        )
        .await;
        write_provenance_log(
            &changepacks_dir,
            &format!("{CARRY_FORWARD_LOG_PREFIX}source.json"),
            "source/package.json",
            UpdateType::Patch,
            "persisted generated source",
        )
        .await;
        let config = Config {
            update_on: BTreeMap::from([(
                "seed/package.json".to_string(),
                vec!["target/package.json".to_string()],
            )]),
            ..Default::default()
        };

        // Control: the target alone is fresh-generated and does carry forward.
        let mut control = gen_update_map(&changepacks_dir, &config).await.unwrap();
        assert_eq!(
            carry_forward_for(
                &mut control,
                &["source/package.json", "target/package.json"]
            ),
            vec![(
                PathBuf::from("target/package.json"),
                update_on_note("seed/package.json")
            )]
        );

        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();
        plan.merge_provenance(&[(
            PathBuf::from("source/package.json"),
            PathBuf::from("target/package.json"),
        )]);

        assert!(
            carry_forward_for(&mut plan, &["source/package.json", "target/package.json"])
                .is_empty()
        );
    }

    // The seed half of `merge_provenance`: the source's expansion-seed
    // membership moves to the target, so reverse-dependency expansion now
    // starts from the workspace root that owns the bump and no longer from the
    // folded member. Complements
    // `persisted_carry_plan_does_not_seed_update_on_or_reverse_dependencies`,
    // which pins that a non-seed path expands nothing.
    #[tokio::test]
    async fn merge_provenance_rekeys_expansion_seed_to_target() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        // Explicit -> `member/package.json` is an expansion seed.
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_member.json",
            "member/package.json",
            UpdateType::Minor,
            "explicit member",
        )
        .await;
        // Carry-forward -> `core/package.json` is scheduled but NOT a seed.
        write_provenance_log(
            &changepacks_dir,
            &format!("{CARRY_FORWARD_LOG_PREFIX}core.json"),
            "core/package.json",
            UpdateType::Patch,
            "persisted generated core",
        )
        .await;
        let config = Config::default();
        let core = create_project("core", vec![]);
        let member = create_project("member", vec![]);
        let core_dependent = create_project("core-dependent", vec!["core"]);
        let member_dependent = create_project("member-dependent", vec!["member"]);
        let projects: Vec<&Project> = vec![&core, &member, &core_dependent, &member_dependent];

        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();
        plan.merge_provenance(&[(
            PathBuf::from("member/package.json"),
            PathBuf::from("core/package.json"),
        )]);
        plan.apply_reverse_dependencies(&projects, Path::new("/test"))
            .unwrap();

        // The seed moved: `core` now expands, `member` no longer does.
        assert!(plan.contains_key(Path::new("core-dependent/package.json")));
        assert!(!plan.contains_key(Path::new("member-dependent/package.json")));
    }

    // `retain_updates` is the plan's only pruning entry point (`changepacks
    // update` calls it to drop the projects the user deselected). Its first
    // contract is plain map surgery: every excluded key leaves the `Deref`'d
    // update map, and every retained key survives with its update type and
    // accumulated result logs untouched.
    #[tokio::test]
    async fn retain_updates_removes_excluded_and_keeps_retained_entries() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_keep.json",
            "keep/package.json",
            UpdateType::Minor,
            "explicit keep",
        )
        .await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_drop.json",
            "drop/package.json",
            UpdateType::Major,
            "explicit drop",
        )
        .await;
        let mut plan = gen_update_map(&changepacks_dir, &Config::default())
            .await
            .unwrap();
        assert_eq!(plan.len(), 2);

        plan.retain_updates(|path| path == Path::new("keep/package.json"));

        assert_eq!(plan.len(), 1);
        assert!(!plan.contains_key(Path::new("drop/package.json")));
        let (update_type, logs) = &plan[Path::new("keep/package.json")];
        assert_eq!(*update_type, UpdateType::Minor);
        assert_eq!(logs.len(), 1);
    }

    // Provenance alone decides emission, and only `Generated { Fresh }` qualifies.
    // An `Explicit` bump belongs to a changepack log the user still owns, so
    // re-emitting it would double-count that bump on the next run; a
    // `Generated { Persisted }` bump already lives on disk as a
    // `changepack_log_carry_forward_*.json`, so re-emitting it would duplicate
    // the log. Dropping either filter silently double-applies a pending bump.
    #[tokio::test]
    async fn retain_updates_emits_carry_forward_only_for_fresh_generated_provenance() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_explicit.json",
            "explicit/package.json",
            UpdateType::Minor,
            "explicit bump",
        )
        .await;
        write_provenance_log(
            &changepacks_dir,
            &format!("{CARRY_FORWARD_LOG_PREFIX}persisted.json"),
            "persisted/package.json",
            UpdateType::Patch,
            "persisted generated bump",
        )
        .await;
        // The updateOn rule is the only way to mint `Generated { Fresh }`: it has
        // no on-disk representation.
        let config = Config {
            update_on: BTreeMap::from([(
                "explicit/package.json".to_string(),
                vec!["fresh/package.json".to_string()],
            )]),
            ..Default::default()
        };
        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();
        assert_eq!(plan.len(), 3);

        assert_eq!(
            carry_forward_for(
                &mut plan,
                &[
                    "explicit/package.json",
                    "fresh/package.json",
                    "persisted/package.json",
                ],
            ),
            vec![(
                PathBuf::from("fresh/package.json"),
                update_on_note("explicit/package.json"),
            )]
        );
        // All three were excluded, so the pruning half ran for every provenance.
        assert!(plan.is_empty());
    }

    // Each emitted log names exactly the removed path, mapped to the update type
    // the plan HELD for it — not the `Patch` the updateOn rule originally minted.
    // A later `DerefMut` write (what `merge_workspace_inherited_updates` does
    // when a workspace root absorbs a member's bump) must therefore be reflected,
    // or the carried-forward bump would silently downgrade on the next run.
    #[tokio::test]
    async fn retain_updates_carry_forward_log_names_only_the_removed_path_and_type() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_seed.json",
            "seed/package.json",
            UpdateType::Minor,
            "explicit seed",
        )
        .await;
        let config = Config {
            update_on: BTreeMap::from([(
                "seed/package.json".to_string(),
                vec!["fresh/package.json".to_string()],
            )]),
            ..Default::default()
        };
        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();
        let fresh = PathBuf::from("fresh/package.json");
        assert_eq!(plan[&fresh].0, UpdateType::Patch);
        plan.get_mut(&fresh)
            .expect("the updateOn rule scheduled the dependent")
            .0 = UpdateType::Major;

        let logs = plan.retain_updates(|path| path != fresh.as_path());

        assert_eq!(logs.len(), 1);
        assert_eq!(
            *logs[0].changes(),
            BTreeMap::from([(fresh.clone(), UpdateType::Major)])
        );
        assert_eq!(logs[0].note(), update_on_note("seed/package.json"));
        // The retained seed keeps its own bump and emits nothing.
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[Path::new("seed/package.json")].0, UpdateType::Minor);
    }

    // Multiple exclusions are emitted in `compare_paths` order, which normalizes
    // backslashes before comparing. `zz\alpha/...` therefore precedes
    // `zz/beta/...`, the opposite of raw byte order ('/' is 0x2F, '\' is 0x5C) —
    // so a lost normalization or a lost sort reorders this vector.
    #[tokio::test]
    async fn retain_updates_emits_carry_forward_logs_in_compare_paths_order() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = provenance_fixture_dir(&temp_dir).await;
        write_provenance_log(
            &changepacks_dir,
            "changepack_log_seed.json",
            "seed/package.json",
            UpdateType::Minor,
            "explicit seed",
        )
        .await;
        let config = Config {
            update_on: BTreeMap::from([(
                "seed/package.json".to_string(),
                vec![
                    "zz/beta/package.json".to_string(),
                    "zz\\alpha/package.json".to_string(),
                    "aa/gamma/package.json".to_string(),
                ],
            )]),
            ..Default::default()
        };
        let mut plan = gen_update_map(&changepacks_dir, &config).await.unwrap();

        let note = update_on_note("seed/package.json");
        assert_eq!(
            carry_forward_for(
                &mut plan,
                &[
                    "zz/beta/package.json",
                    "zz\\alpha/package.json",
                    "aa/gamma/package.json",
                ],
            ),
            vec![
                (PathBuf::from("aa/gamma/package.json"), note.clone()),
                (PathBuf::from("zz\\alpha/package.json"), note.clone()),
                (PathBuf::from("zz/beta/package.json"), note),
            ]
        );
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

    // Locks in the fast-path added to `apply_update_on_rules`: an empty
    // `update_map` combined with a non-empty `updateOn` config must remain
    // empty (no side effect). A future refactor that reorders the two
    // guards — or drops the `update_map.is_empty()` check — would flip
    // this test red immediately. Companion to the existing "no match" /
    // "invalid pattern" cases below.
    #[test]
    fn test_apply_update_on_rules_empty_update_map_is_noop() {
        let mut update_on = BTreeMap::new();
        update_on.insert("crates/*".to_string(), vec!["bridge/node".to_string()]);
        let config = Config {
            update_on,
            ..Default::default()
        };

        let mut update_map: HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)> =
            HashMap::new();

        apply_update_on_rules(&mut update_map, &config).unwrap();

        assert!(
            update_map.is_empty(),
            "empty update_map + non-empty updateOn config must stay empty (fast-path violated)"
        );
    }

    #[tokio::test]
    async fn test_gen_update_map_invalid_update_on_pattern_is_error() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();
        let mut update_on = BTreeMap::new();
        update_on.insert("[invalid".to_string(), vec!["bridge/node".to_string()]);
        let config = Config {
            update_on,
            ..Default::default()
        };

        let error = gen_update_map(&changepacks_dir, &config)
            .await
            .expect_err("invalid updateOn patterns must be rejected");

        assert!(
            error
                .to_string()
                .contains("invalid glob pattern in updateOn config: [invalid")
        );
    }

    #[test]
    fn test_apply_update_on_rules_normalizes_candidate_separators() {
        assert_eq!(
            normalize_path_separators_of(Path::new("crates/core/Cargo.toml")),
            "crates/core/Cargo.toml"
        );
        assert_eq!(
            normalize_path_separators_of(Path::new(r"crates\core\Cargo.toml")),
            "crates/core/Cargo.toml"
        );

        let config = Config {
            update_on: BTreeMap::from([
                (
                    "bridge/*".to_string(),
                    vec!["release/Cargo.toml".to_string()],
                ),
                (
                    "crates/*".to_string(),
                    vec!["bridge/node/Cargo.toml".to_string()],
                ),
            ]),
            ..Default::default()
        };

        for candidate in ["crates/core/Cargo.toml", r"crates\core\Cargo.toml"] {
            let candidate_path = PathBuf::from(candidate);
            let unrelated_path = PathBuf::from("docs/guide.md");
            let mut update_map = HashMap::from([
                (candidate_path.clone(), (UpdateType::Minor, vec![])),
                (unrelated_path.clone(), (UpdateType::Major, vec![])),
            ]);

            apply_update_on_rules(&mut update_map, &config).unwrap();

            assert_eq!(update_map.len(), 4, "candidate: {candidate}");
            assert!(
                update_map.contains_key(&candidate_path),
                "native candidate key must remain unchanged: {candidate}"
            );
            assert_eq!(
                update_map[&unrelated_path].0,
                UpdateType::Major,
                "unrelated paths must remain unmatched: {candidate}"
            );
            assert!(
                update_map[&unrelated_path].1.is_empty(),
                "unrelated paths must not receive updateOn notes: {candidate}"
            );
            assert_eq!(
                update_map[Path::new("bridge/node/Cargo.toml")].0,
                UpdateType::Patch,
                "candidate: {candidate}"
            );
            assert_eq!(
                update_map[Path::new("release/Cargo.toml")].0,
                UpdateType::Patch,
                "transitive rules must converge: {candidate}"
            );
        }
    }

    #[test]
    fn test_apply_update_on_rules_no_match() {
        // Test when no package matches the trigger pattern
        let mut update_on = BTreeMap::new();
        update_on.insert("other/*".to_string(), vec!["bridge/node".to_string()]);
        let config = Config {
            update_on,
            ..Default::default()
        };

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_update_on_rules(&mut update_map, &config).unwrap();

        // Should still have only the original entry (no match)
        assert_eq!(update_map.len(), 1);
    }

    #[test]
    fn test_apply_update_on_rules_dependent_already_exists() {
        // Test when dependent package is already in update_map
        let mut update_on = BTreeMap::new();
        update_on.insert("crates/*".to_string(), vec!["bridge/node".to_string()]);
        let config = Config {
            update_on,
            ..Default::default()
        };

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );
        update_map.insert(
            PathBuf::from("bridge/node"),
            (
                UpdateType::Major,
                vec![ChangePackResultLog::new(
                    UpdateType::Major,
                    "Update bridge".to_string(),
                )],
            ),
        );

        apply_update_on_rules(&mut update_map, &config).unwrap();

        // bridge/node should remain Major (not overwritten to Patch)
        assert_eq!(update_map.len(), 2);
        assert_eq!(
            update_map[&PathBuf::from("bridge/node")].0,
            UpdateType::Major
        );
    }

    // Two updateOn triggers whose globs BOTH match the same updated package
    // ("crates/core") and BOTH list the same dependent ("bridge/node"). With a
    // BTreeMap-backed `update_on`, `apply_update_on_rules` iterates triggers in
    // sorted key order and inserts the dependent exactly once — on the FIRST
    // matching trigger — so the auto-update note deterministically names the
    // lexicographically-smallest trigger key. Under the previous HashMap the
    // winning trigger (and thus the note text) was nondeterministic per run.
    #[test]
    fn test_apply_update_on_rules_names_lexicographically_first_trigger() {
        // Both "crates/*" and "crates/core" match "crates/core"; byte-wise
        // '*' (0x2A) < 'c' (0x63), so "crates/*" is the smaller key and must
        // be the one named in the note.
        let mut update_on = BTreeMap::new();
        update_on.insert("crates/core".to_string(), vec!["bridge/node".to_string()]);
        update_on.insert("crates/*".to_string(), vec!["bridge/node".to_string()]);
        let config = Config {
            update_on,
            ..Default::default()
        };

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_update_on_rules(&mut update_map, &config).unwrap();

        // The dependent is added exactly once despite two matching triggers.
        assert_eq!(update_map.len(), 2);
        let dependent = &update_map[&PathBuf::from("bridge/node")];
        assert_eq!(dependent.0, UpdateType::Patch);
        assert_eq!(dependent.1.len(), 1);

        // The note names the lexicographically-first trigger key, which is
        // exactly `update_on`'s first key under BTreeMap ordering.
        let first_trigger = config.update_on.keys().next().unwrap();
        assert_eq!(first_trigger, "crates/*");
        let note_json = serde_json::to_value(&dependent.1[0]).unwrap();
        assert_eq!(
            note_json["note"],
            format!("Auto-update triggered by updateOn rule: {first_trigger}")
        );
    }

    #[test]
    fn test_apply_update_on_rules_reaches_chained_rules() {
        let config = Config {
            update_on: BTreeMap::from([
                ("packages/a".to_string(), vec!["packages/b".to_string()]),
                ("packages/b".to_string(), vec!["packages/c".to_string()]),
            ]),
            ..Default::default()
        };
        let mut update_map =
            HashMap::from([(PathBuf::from("packages/a"), (UpdateType::Minor, vec![]))]);

        apply_update_on_rules(&mut update_map, &config).unwrap();

        assert_eq!(update_map.len(), 3);
        assert_eq!(update_map[Path::new("packages/b")].0, UpdateType::Patch);
        assert_eq!(update_map[Path::new("packages/c")].0, UpdateType::Patch);
    }

    #[test]
    fn test_apply_update_on_rules_reaches_diamond_once() {
        let config = Config {
            update_on: BTreeMap::from([
                (
                    "packages/a".to_string(),
                    vec!["packages/b".to_string(), "packages/c".to_string()],
                ),
                ("packages/b".to_string(), vec!["packages/d".to_string()]),
                ("packages/c".to_string(), vec!["packages/d".to_string()]),
            ]),
            ..Default::default()
        };
        let mut update_map =
            HashMap::from([(PathBuf::from("packages/a"), (UpdateType::Minor, vec![]))]);

        apply_update_on_rules(&mut update_map, &config).unwrap();

        assert_eq!(update_map.len(), 4);
        let diamond = &update_map[Path::new("packages/d")];
        assert_eq!(diamond.1.len(), 1);
        assert_eq!(
            serde_json::to_value(&diamond.1[0]).unwrap()["note"],
            "Auto-update triggered by updateOn rule: packages/b"
        );
    }

    #[test]
    fn test_apply_update_on_rules_terminates_cycles() {
        let config = Config {
            update_on: BTreeMap::from([
                ("packages/a".to_string(), vec!["packages/b".to_string()]),
                ("packages/b".to_string(), vec!["packages/a".to_string()]),
            ]),
            ..Default::default()
        };
        let mut update_map =
            HashMap::from([(PathBuf::from("packages/a"), (UpdateType::Minor, vec![]))]);

        apply_update_on_rules(&mut update_map, &config).unwrap();

        assert_eq!(update_map.len(), 2);
        assert_eq!(update_map[Path::new("packages/a")].0, UpdateType::Minor);
        assert_eq!(update_map[Path::new("packages/b")].0, UpdateType::Patch);
    }

    #[test]
    fn test_apply_update_on_rules_propagates_from_already_present_dependent() {
        let config = Config {
            update_on: BTreeMap::from([
                ("packages/a".to_string(), vec!["packages/b".to_string()]),
                ("packages/b".to_string(), vec!["packages/c".to_string()]),
            ]),
            ..Default::default()
        };
        let mut update_map = HashMap::from([
            (PathBuf::from("packages/a"), (UpdateType::Minor, vec![])),
            (PathBuf::from("packages/b"), (UpdateType::Major, vec![])),
        ]);

        apply_update_on_rules(&mut update_map, &config).unwrap();

        assert_eq!(update_map.len(), 3);
        assert_eq!(update_map[Path::new("packages/b")].0, UpdateType::Major);
        assert_eq!(update_map[Path::new("packages/c")].0, UpdateType::Patch);
    }
}
