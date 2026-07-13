use std::{
    borrow::Cow,
    collections::{HashMap, hash_map::Entry},
    hash::BuildHasher,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use changepacks_core::{ChangePackLog, ChangePackResultLog, Config, Project, UpdateType};
use glob::Pattern;

use crate::{collect_changepack_log_paths, get_relative_path, read_log_bodies};

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
    let mut update_map: HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)> =
        HashMap::with_capacity(paths.len());
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
    let trigger_matches: Vec<(&str, &[String])> = {
        // Preallocate: `HashMap::keys()` yields an `ExactSizeIterator`
        // whose `size_hint = (len, Some(len))`, and `Vec::from_iter` DOES
        // reserve against the exact upper bound in that case — so the
        // previous `.collect()` was already zero-realloc. Switch to the
        // explicit `Vec::with_capacity(update_map.len()) + .extend(...)`
        // form purely for visual consistency with the `trigger_matches`
        // preallocation a few lines below and the identical policy
        // applied to `path_to_name` / `reverse_deps` / `packages_to_add`
        // in the sibling `apply_reverse_dependencies`. Byte-identical
        // output; the goal is a uniform preallocation idiom across this
        // module so a future maintainer can trust every `Vec::from_iter`
        // is deliberate.
        let mut updated_paths: Vec<Cow<'_, str>> = Vec::with_capacity(update_map.len());
        updated_paths.extend(update_map.keys().map(|p| p.to_string_lossy()));

        let mut out = Vec::with_capacity(config.update_on.len());
        for (trigger_pattern, dependents) in &config.update_on {
            match Pattern::new(trigger_pattern) {
                Ok(pattern) => {
                    if updated_paths.iter().any(|s| pattern.matches(s.as_ref())) {
                        out.push((trigger_pattern.as_str(), dependents.as_slice()));
                    }
                }
                Err(_) => {
                    eprintln!(
                        "warning: invalid glob pattern in updateOn config: {trigger_pattern}"
                    );
                }
            }
        }
        out
    };

    for (trigger_pattern, dependents) in trigger_matches {
        // Add dependent packages as PATCH updates if not already in update_map
        for dependent in dependents {
            // Guard with a borrowed `Path` lookup so the `PathBuf` key is only
            // allocated on the insert (cache-miss) path.
            if !update_map.contains_key(Path::new(dependent)) {
                update_map.insert(
                    PathBuf::from(dependent),
                    (
                        UpdateType::Patch,
                        vec![ChangePackResultLog::new(
                            UpdateType::Patch,
                            format!("Auto-update triggered by updateOn rule: {trigger_pattern}"),
                        )],
                    ),
                );
            }
        }
    }
}

/// Apply reverse dependency updates: if package A depends on package B (via a local workspace dependency),
/// and B is being updated, then A should also be updated as PATCH.
pub fn apply_reverse_dependencies<S: BuildHasher>(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>), S>,
    projects: &[&Project],
    repo_root_path: &Path,
) -> Result<()> {
    // Fast path for the dominant no-op case: with no scheduled updates the
    // seed set below is empty and the reverse-dep DFS discovers nothing, so
    // walking the full project graph to populate `path_to_name` /
    // `reverse_deps` is pure waste (N `String` + N `PathBuf` allocations per
    // project). Semantic mirror of the existing guard in
    // `apply_update_on_rules` above.
    if update_map.is_empty() {
        return Ok(());
    }

    // Second fast path: with no scheduled updates carrying any local
    // (monorepo) dependency edge, the DFS discovers nothing and the
    // full `path_to_name` / `reverse_deps` construction below is pure
    // waste (N `PathBuf` + N `String` allocs per project). Common on
    // single-package repos and workspaces that don't use `workspace:*`
    // / `workspace = true` / `[tool.uv.sources]` / `<ProjectReference/>`.
    // `.all(...)` short-circuits on the first dep-carrying project, so
    // it's O(1) amortized when the rest of the function would have
    // fired anyway.
    if projects.iter().all(|p| p.dependencies().is_empty()) {
        return Ok(());
    }

    // Dependencies are resolved by project name, so record whether every name
    // is unique before building either lookup. As in `sort_by_dependencies`,
    // `Some(index)` means unique and `None` means duplicate/ambiguous.
    let mut name_to_index: HashMap<&str, Option<usize>> = HashMap::with_capacity(projects.len());
    for (idx, project) in projects.iter().enumerate() {
        if let Some(name) = project.name() {
            match name_to_index.entry(name) {
                Entry::Occupied(entry) => {
                    *entry.into_mut() = None;
                }
                Entry::Vacant(entry) => {
                    entry.insert(Some(idx));
                }
            }
        }
    }

    // Single pass over projects to build:
    //   - path_to_name:   relative file path -> package name (for O(1) reverse lookup)
    //   - reverse_deps:   unique dependency name -> [packages that depend on it]
    let mut path_to_name: HashMap<PathBuf, &str> = HashMap::with_capacity(projects.len());
    let mut reverse_deps: HashMap<&str, Vec<(PathBuf, Option<&str>)>> =
        HashMap::with_capacity(projects.len());
    for (idx, project) in projects.iter().enumerate() {
        let rel_path_buf =
            get_relative_path(repo_root_path, project.path()).with_context(|| {
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
            Some(name) if name_to_index.get(name) == Some(&Some(idx)) => Some(name),
            Some(_) => None,
            None => Some(project_name),
        };

        let dependencies = project.dependencies();
        for dep_name in dependencies {
            if !matches!(name_to_index.get(dep_name.as_str()), Some(Some(_))) {
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
            entry.push((rel_path_buf.clone(), worklist_name));
        }

        // Move `rel_path_buf` into its final consumer instead of cloning it:
        // the edge loop above only borrows it (one clone per edge), so once
        // the loop ends the buffer is free to move — the "move into the last
        // consumer" idiom from `find_project_dirs`'s repo-name fallback.
        if let Some(unique_name) = worklist_name.filter(|_| name_opt.is_some()) {
            path_to_name.insert(rel_path_buf, unique_name);
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
    let mut packages_to_add: HashMap<PathBuf, &str> = HashMap::with_capacity(projects.len());

    // Seed the DFS with names already scheduled for update. The DFS guards
    // against reprocessing through `update_map` and `packages_to_add`.
    // Names stay borrowed `&str` end to end — through the worklist and into
    // `packages_to_add` — so the `PathBuf` keys are the only thing cloned.
    let mut to_process: Vec<&str> = Vec::with_capacity(update_map.len());
    to_process.extend(
        update_map
            .keys()
            .filter_map(|path| path_to_name.get(path).copied()),
    );
    while let Some(trigger_name) = to_process.pop() {
        if let Some(dependents) = reverse_deps.get(trigger_name) {
            for (dependent_path, dependent_name) in dependents {
                if !update_map.contains_key(dependent_path)
                    && !packages_to_add.contains_key(dependent_path)
                {
                    packages_to_add.insert(dependent_path.clone(), trigger_name);
                    if let Some(dependent_name) = dependent_name {
                        to_process.push(*dependent_name);
                    }
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
                    format!(
                        "Auto-update: depends on '{dependency_name}' via a local workspace dependency"
                    ),
                )],
            )
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use changepacks_core::{Config, Package};
    use changepacks_node::package::NodePackage;
    use tempfile::TempDir;
    use tokio::fs;

    use super::*;

    use crate::test_support::create_project;

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
    fn test_apply_reverse_dependencies_duplicate_names_are_isolated() {
        let core = create_project("core", vec![]);
        let mut shared_a = create_project("shared-a", vec!["core"]);
        shared_a.set_name("shared".to_string());
        let mut shared_b = create_project("shared-b", vec!["core"]);
        shared_b.set_name("shared".to_string());
        let app = create_project("app", vec!["shared"]);
        let projects: Vec<&Project> = vec![&core, &shared_a, &shared_b, &app];
        let repo_root = Path::new("/test");

        let mut duplicate_seed = HashMap::new();
        duplicate_seed.insert(
            PathBuf::from("shared-a/package.json"),
            (UpdateType::Minor, vec![]),
        );
        apply_reverse_dependencies(&mut duplicate_seed, &projects, repo_root).unwrap();
        assert!(!duplicate_seed.contains_key(&PathBuf::from("app/package.json")));

        let mut unique_seed = HashMap::new();
        unique_seed.insert(
            PathBuf::from("core/package.json"),
            (UpdateType::Minor, vec![]),
        );
        apply_reverse_dependencies(&mut unique_seed, &projects, repo_root).unwrap();

        assert!(unique_seed.contains_key(&PathBuf::from("shared-a/package.json")));
        assert!(unique_seed.contains_key(&PathBuf::from("shared-b/package.json")));
        assert!(!unique_seed.contains_key(&PathBuf::from("app/package.json")));
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

        apply_update_on_rules(&mut update_map, &config);

        assert!(
            update_map.is_empty(),
            "empty update_map + non-empty updateOn config must stay empty (fast-path violated)"
        );
    }

    #[test]
    fn test_apply_update_on_rules_invalid_pattern() {
        // Test with invalid glob pattern
        let mut update_on = BTreeMap::new();
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

        apply_update_on_rules(&mut update_map, &config);

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

        apply_update_on_rules(&mut update_map, &config);

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

        apply_update_on_rules(&mut update_map, &config);

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
}
