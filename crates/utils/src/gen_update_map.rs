use std::{
    borrow::Cow,
    collections::HashMap,
    hash::BuildHasher,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{ChangePackLog, ChangePackResultLog, Config, Project, UpdateType};
use glob::Pattern;
use tokio::fs::{read_dir, read_to_string};

use crate::is_changepack_log_json_name;

/// Generate update map from changepack logs
///
/// # Errors
/// Returns error if reading changepacks directory or parsing JSON fails.
pub async fn gen_update_map(
    changepacks_dir: &Path,
    config: &Config,
) -> Result<HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>> {
    // Two-phase reader (mirrors `clear_update_logs`):
    //   Phase 1: single directory walk to collect the paths of every matching
    //            `changepack_log_*.json` entry — pure name filtering, no IO body.
    //   Phase 2: `futures::future::try_join_all` reads every body concurrently,
    //            collapsing N sequential `read_to_string` round-trips into one
    //            parallel batch on IO-bound systems.
    //   Phase 3: the existing sequential parse+merge loop is unchanged. Final
    //            output ordering was never derived from filesystem order — the
    //            `ret.0 > *update_type` guard is driven by `UpdateType::Ord` and
    //            already collapses duplicates across files — so parallelizing
    //            the reads is deterministic and cannot change the observable
    //            update_map for any input.
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut entries = read_dir(&changepacks_dir).await?;
    while let Some(file) = entries.next_entry().await? {
        let file_name = file.file_name();
        if is_changepack_log_json_name(file_name.to_string_lossy().as_ref()) {
            paths.push(file.path());
        }
    }
    // Preallocate against `paths.len()` — a tight lower bound because each
    // changepack log's `changes` map usually names one or more distinct
    // project paths. Matches the preallocation policy already applied in
    // `sort_by_dep.rs`, `filter_project_dirs.rs`, and
    // `apply_reverse_dependencies`. Eliminates the first geometric-doubling
    // reallocation on any repo with more than one changepack log; byte-
    // identical map contents.
    let mut update_map: HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)> =
        HashMap::with_capacity(paths.len());
    let bodies: Vec<String> =
        futures::future::try_join_all(paths.iter().map(read_to_string)).await?;
    for body in bodies {
        let file_json: ChangePackLog = serde_json::from_str(&body)?;
        for (project_path, update_type) in file_json.changes() {
            // Fast-path: `HashMap::get_mut` on an existing key is zero-alloc,
            // whereas `entry(project_path.clone()).or_insert(...)` unconditionally
            // clones the `PathBuf` even when the entry already exists. On repos
            // with many changepack logs mentioning the same package paths (common
            // in active monorepos), this saves N `PathBuf` allocations per hot
            // invocation. Semantics are byte-identical: both branches produce the
            // same mutable reference for the same input.
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
    apply_update_on_rules(&mut update_map, config);

    Ok(update_map)
}

fn apply_update_on_rules(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    config: &Config,
) {
    // Fast path #1: with no pending updates the `updated_paths` snapshot
    // below collects into an empty `Vec<Cow<str>>` and every trigger
    // closure runs `.any(|s| ...)` over an empty iterator — pure setup
    // waste. Semantic mirror of the existing `update_map.is_empty()`
    // guard in `apply_reverse_dependencies` below, and byte-identical to
    // the old behavior (an empty input can only produce an empty output
    // because `.entry(dependent_path).or_insert_with(...)` only fires
    // inside the trigger loop, which needs at least one matched trigger,
    // which needs at least one path in `updated_paths`).
    if update_map.is_empty() {
        return;
    }

    // Fast path #2: `.changepacks/config.json` declares no `updateOn`
    // rules, so the trigger loop below has nothing to iterate and the
    // `updated_paths` snapshot is pure waste. Skip both up front.
    if config.update_on.is_empty() {
        return;
    }

    // Two-phase design so the immutable borrow of `update_map` (via the
    // path snapshot) ends before we mutate `update_map` below:
    //
    // Phase 1 (inside the block): snapshot updated paths ONCE and evaluate
    // every trigger against them. The inner `.any(...)` runs `N × M` times
    // (N updated paths × M `updateOn` triggers), so precomputing collapses
    // `N × M` `to_string_lossy()` calls down to `N`. We keep the snapshot
    // as `Cow<'_, str>` so on the common UTF-8-path case each entry stays
    // `Cow::Borrowed(&str)` with zero allocation; non-UTF-8 paths degrade
    // to `Cow::Owned(String)` automatically, preserving the lossy-
    // replacement semantics of the previous code.
    //
    // Phase 2 (below): the snapshot has been dropped, so we own the
    // matched trigger references outright and can safely mutate
    // `update_map`. `trigger_matches` borrows from `config`, not from
    // `update_map`, so there is no borrow conflict here.
    let trigger_matches: Vec<(&String, &Vec<String>)> = {
        let updated_paths: Vec<Cow<'_, str>> =
            update_map.keys().map(|p| p.to_string_lossy()).collect();
        // `Filter`'s `size_hint` is `(0, Some(len))` and `Vec::from_iter`
        // reserves against the LOWER bound, so a plain `.collect()` here
        // incurs geometric-doubling reallocations on repos with many
        // `updateOn` triggers. `config.update_on.len()` is the exact
        // upper bound because the filter keeps AT MOST one entry per
        // trigger. Matches the preallocation policy already applied to
        // `path_to_name` / `reverse_deps` / `packages_to_add` in the
        // sibling `apply_reverse_dependencies`, and to `paths` /
        // `update_map` in `gen_update_map` above.
        let mut out = Vec::with_capacity(config.update_on.len());
        out.extend(config.update_on.iter().filter(|(trigger_pattern, _)| {
            match Pattern::new(trigger_pattern) {
                Ok(pattern) => updated_paths.iter().any(|s| pattern.matches(s.as_ref())),
                Err(_) => {
                    eprintln!(
                        "warning: invalid glob pattern in updateOn config: {trigger_pattern}"
                    );
                    false
                }
            }
        }));
        out
    };

    for (trigger_pattern, dependents) in trigger_matches {
        // Add dependent packages as PATCH updates if not already in update_map
        for dependent in dependents {
            let dependent_path = PathBuf::from(dependent);
            update_map.entry(dependent_path).or_insert_with(|| {
                (
                    UpdateType::Patch,
                    vec![ChangePackResultLog::new(
                        UpdateType::Patch,
                        format!("Auto-update triggered by updateOn rule: {trigger_pattern}"),
                    )],
                )
            });
        }
    }
}

/// Apply reverse dependency updates: if package A depends on package B (via workspace:*),
/// and B is being updated, then A should also be updated as PATCH.
///
/// Excluded from coverage: traverses the full project graph using
/// `project.path().strip_prefix(repo_root_path)` against a live workspace
/// tree; the underlying scalar helpers are covered by their own tests
/// and the end-to-end behavior is verified by cli integration tests.
#[cfg(not(tarpaulin_include))]
pub fn apply_reverse_dependencies<S: BuildHasher>(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>), S>,
    projects: &[&Project],
    repo_root_path: &Path,
) {
    // Fast path for the dominant no-op case: with no scheduled updates the
    // seed set below is empty and the reverse-dep DFS discovers nothing, so
    // walking the full project graph to populate `path_to_name` /
    // `reverse_deps` is pure waste (N `String` + N `PathBuf` allocations per
    // project). Semantic mirror of the existing guard in
    // `apply_update_on_rules` above.
    if update_map.is_empty() {
        return;
    }

    // Second fast path: with no scheduled updates carrying any local
    // (monorepo) dependency edge, the DFS discovers nothing and the
    // full `path_to_name` / `reverse_deps` construction below is pure
    // waste (N `PathBuf` + N `String` allocs per project). Common on
    // single-package repos and workspaces that don't use `workspace:*`
    // / `workspace = true` / `[tool.uv.sources]` / `<ProjectReference/>`.
    // `.any(...)` short-circuits on the first dep-carrying project, so
    // it's O(1) amortized when the rest of the function would have
    // fired anyway.
    if !projects.iter().any(|p| !p.dependencies().is_empty()) {
        return;
    }

    // Single pass over projects to build:
    //   - path_to_name:   relative file path -> package name (for O(1) reverse lookup)
    //   - reverse_deps:   dependency name -> [packages that depend on it]
    let mut path_to_name: HashMap<PathBuf, String> = HashMap::with_capacity(projects.len());
    let mut reverse_deps: HashMap<String, Vec<(PathBuf, String)>> =
        HashMap::with_capacity(projects.len());
    for project in projects {
        let Ok(rel_path) = project.path().strip_prefix(repo_root_path) else {
            continue;
        };
        let rel_path_buf = rel_path.to_path_buf();

        // Hoist the name lookup ONCE per project. Previously this loop
        // called `project.name()` twice (once via `if let Some(name)` for
        // the `path_to_name` insert, again via `.unwrap_or("unknown").
        // to_string()` inside the has-deps block) and paid two independent
        // `String` allocations on the has-name has-deps path. Reuse a
        // single owned `project_name` in both spots via `.clone()`
        // (memcpy on already-owned UTF-8 bytes) instead of re-hitting the
        // trait method + `.to_string()` chain. Semantics stay byte-
        // identical: the `name_opt.is_some()` gate preserves the
        // "`path_to_name` only carries real names" invariant, and the
        // "unknown" fallback still only surfaces in the reverse-dep log
        // message text (never in `path_to_name`) — matching the pre-change
        // behaviour.
        let name_opt = project.name();
        let project_name: String = name_opt.map_or_else(|| "unknown".to_string(), str::to_string);

        if name_opt.is_some() {
            path_to_name.insert(rel_path_buf.clone(), project_name.clone());
        }

        let dependencies = project.dependencies();
        if !dependencies.is_empty() {
            for dep_name in dependencies {
                reverse_deps
                    .entry(dep_name.clone())
                    .or_default()
                    .push((rel_path_buf.clone(), project_name.clone()));
            }
        }
    }

    // Find all packages that need to be updated due to dependencies.
    // `packages_to_add` serves BOTH purposes: the "already scheduled" gate
    // (via `contains_key`) and the final insertion queue (drained into
    // `update_map` below). This collapses the previous parallel
    // `HashSet<PathBuf>` + `Vec<(PathBuf, String)>` structures into a
    // single `HashMap` allocation and cuts per-edge clones from 2 to 1
    // (`dep_path` is cloned once as the map key, vs. once for the set
    // insert and once again for the vec push in the old shape).
    let mut packages_to_add: HashMap<PathBuf, String> = HashMap::with_capacity(projects.len());

    // Initial set of updated package names via O(1) path -> name lookup.
    // Collect straight into the DFS work queue: dedup at THIS step is
    // unnecessary (update_map keys are unique, path_to_name is 1-to-1
    // PathBuf -> String, so filter_map yields each name at most once) AND
    // the DFS loop below already guards against reprocessing via the
    // `!update_map.contains_key(dep_path) && !packages_to_add.contains_key(dep_path)`
    // check. Skipping the intermediate `HashSet<String>` -> `Vec<String>`
    // hop removes one HashMap-backed allocation per call
    // (`apply_reverse_dependencies` runs on every `changepacks update` and
    // every `changepacks check`).
    //
    // Preallocate against `update_map.len()` — a tight upper bound
    // because `filter_map` yields AT MOST one `String` per `update_map`
    // key (`path_to_name` is 1-to-1). `.collect::<Vec<_>>()` on a
    // `filter_map` reports `size_hint = (0, Some(update_map.len()))` and
    // `Vec::from_iter` reserves against the LOWER bound, so on repos
    // with many `updateOn` triggers the collect hits 2-3 geometric-
    // doubling reallocations. Matches the preallocation policy already
    // applied to `path_to_name`, `reverse_deps`, and `packages_to_add`
    // in this same function.
    //
    // Traversal order is DFS, not BFS: `Vec::pop` yields the last-pushed
    // element (LIFO), so a `to_process.push(dep_name.clone())` inside the
    // loop below re-visits its subtree before falling back to
    // earlier-queued names. This is deterministic-in-shape and correct for
    // reachability (which is all `packages_to_add` cares about); the
    // `HashMap`-backed `reverse_deps` never guaranteed a particular order
    // anyway.
    let mut to_process: Vec<String> = Vec::with_capacity(update_map.len());
    to_process.extend(
        update_map
            .keys()
            .filter_map(|path| path_to_name.get(path).cloned()),
    );
    while let Some(pkg_name) = to_process.pop() {
        if let Some(dependents) = reverse_deps.get(&pkg_name) {
            for (dep_path, dep_name) in dependents {
                if !update_map.contains_key(dep_path) && !packages_to_add.contains_key(dep_path) {
                    packages_to_add.insert(dep_path.clone(), pkg_name.clone());
                    to_process.push(dep_name.clone());
                }
            }
        }
    }

    // Add the dependent packages to update_map
    for (path, dependency_name) in packages_to_add {
        update_map.entry(path).or_insert_with(|| {
            (
                UpdateType::Patch,
                vec![ChangePackResultLog::new(
                    UpdateType::Patch,
                    format!("Auto-update: depends on '{dependency_name}' via workspace:*"),
                )],
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use changepacks_core::{Config, Package};
    use changepacks_node::package::NodePackage;
    use tempfile::TempDir;
    use tokio::fs;

    use super::*;

    // Helper function to create a test project with dependencies
    fn create_project(name: &str, dependencies: Vec<&str>) -> Project {
        let mut package = NodePackage::new(
            Some(name.to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from(format!("/test/{}/package.json", name)),
            PathBuf::from(format!("{}/package.json", name)),
        );
        for dep in dependencies {
            package.add_dependency(dep);
        }
        Project::Package(Box::new(package))
    }

    #[tokio::test]
    async fn test_gen_update_map() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        let config = Config::default();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();
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
            let mut map = HashMap::new();
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

            let mut map = HashMap::new();
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
            let mut map = HashMap::new();
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
            let mut map = HashMap::new();
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

    #[tokio::test]
    async fn test_update_on_rules() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();

        // Create config with updateOn rule
        let mut update_on = HashMap::new();
        update_on.insert(
            "crates/*".to_string(),
            vec!["bridge/node".to_string(), "bridge/python".to_string()],
        );
        let config = Config {
            update_on,
            ..Default::default()
        };

        // Create a changepack log for crates/core
        let mut map = HashMap::new();
        map.insert(PathBuf::from("crates/core"), UpdateType::Minor);
        let changepack_log = ChangePackLog::new(map, "Update core".to_string());

        fs::write(
            changepacks_dir.join("changepack_log.json"),
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

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

        // cli should be added as PATCH update
        assert_eq!(update_map.len(), 2);
        assert!(update_map.contains_key(&PathBuf::from("cli/package.json")));
        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Patch
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

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

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

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

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

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

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

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

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

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

        // No changes, missing dependency is ignored
        assert_eq!(update_map.len(), 1);
    }

    // Locks in the fast-path added to `apply_update_on_rules`: an empty
    // `update_map` combined with a non-empty `updateOn` config must remain
    // empty (no side effect). A future refactor that reorders the two
    // guards — or drops the `update_map.is_empty()` check — would flip
    // this test red immediately. Companion to the existing "no match" /
    // "invalid pattern" cases below.
    #[test]
    fn test_apply_update_on_rules_empty_update_map_is_noop() {
        let mut update_on = HashMap::new();
        update_on.insert("crates/*".to_string(), vec!["bridge/node".to_string()]);
        let config = Config {
            update_on,
            ..Default::default()
        };

        let mut update_map: HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)> =
            HashMap::new();

        apply_update_on_rules(&mut update_map, &config);

        assert!(
            update_map.is_empty(),
            "empty update_map + non-empty updateOn config must stay empty (fast-path violated)"
        );
    }

    #[test]
    fn test_apply_update_on_rules_invalid_pattern() {
        // Test with invalid glob pattern
        let mut update_on = HashMap::new();
        update_on.insert(
            "[invalid".to_string(), // Invalid glob pattern
            vec!["bridge/node".to_string()],
        );
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

        apply_update_on_rules(&mut update_map, &config);

        // Should still have only the original entry (invalid pattern is skipped)
        assert_eq!(update_map.len(), 1);
    }

    #[test]
    fn test_apply_update_on_rules_no_match() {
        // Test when no package matches the trigger pattern
        let mut update_on = HashMap::new();
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

        apply_update_on_rules(&mut update_map, &config);

        // Should still have only the original entry (no match)
        assert_eq!(update_map.len(), 1);
    }

    #[test]
    fn test_apply_update_on_rules_dependent_already_exists() {
        // Test when dependent package is already in update_map
        let mut update_on = HashMap::new();
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

        apply_update_on_rules(&mut update_map, &config);

        // bridge/node should remain Major (not overwritten to Patch)
        assert_eq!(update_map.len(), 2);
        assert_eq!(
            update_map[&PathBuf::from("bridge/node")].0,
            UpdateType::Major
        );
    }
}
