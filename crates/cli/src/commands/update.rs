use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{
    ChangePackResultLog, Language, Package, Project, ProjectFinder, UpdateType, Workspace,
};
use changepacks_utils::{
    apply_reverse_dependencies, clear_applied_update_logs, clear_update_logs,
    discover_project_dirs, display_update, gen_changepack_result_map, gen_update_map,
    get_relative_path, get_relative_path_ref,
};
use clap::Args;

use crate::{
    CommandContext,
    finders::{collect_projects, get_finders, total_project_count},
    options::{CliLanguage, FormatOptions, language_slice_contains},
    prompter::{InquirePrompter, Prompter},
};

type UpdateProjectMut<'a> = (&'a mut Project, UpdateType);
type UpdateProjectRef<'a> = (&'a Project, UpdateType);
type WorkspaceRef<'a> = &'a dyn Workspace;

#[derive(Args, Debug)]
#[command(about = "Update project versions from changepack logs")]
pub struct UpdateArgs {
    #[arg(short, long)]
    pub dry_run: bool,

    #[arg(short, long)]
    pub yes: bool,

    #[arg(long, default_value = "stdout")]
    pub format: FormatOptions,

    #[arg(short, long)]
    pub remote: bool,

    /// Filter projects by language. Can be specified multiple times to include multiple languages.
    #[arg(short, long, value_enum)]
    pub language: Vec<CliLanguage>,
}

/// Update project version
///
/// # Errors
/// Returns error if command context creation or version update fails.
pub async fn handle_update(args: &UpdateArgs) -> Result<()> {
    handle_update_with_prompter(args, &InquirePrompter).await
}

/// # Errors
/// Returns error if reading changepack logs, updating versions, or writing results fails.
///
/// Excluded from coverage: orchestrates `CommandContext::new` and
/// `find_project_dirs` (real git tree walk) plus an interactive
/// `prompter.confirm(...)`; underlying helpers (`apply_reverse_dependencies`,
/// `gen_update_map`, `display_update`) are covered by their own tests.
#[cfg(not(tarpaulin_include))]
pub async fn handle_update_with_prompter(args: &UpdateArgs, prompter: &dyn Prompter) -> Result<()> {
    let ctx = CommandContext::new(args.remote).await?;
    let mut update_map = gen_update_map(&ctx.changepacks_dir, &ctx.config).await?;

    // Early return if no updates: `apply_update_on_rules` already ran inside
    // `gen_update_map`; `apply_reverse_dependencies` has an `is_empty()` fast-path;
    // `merge_workspace_inherited_updates` only mutates on `contains_key` hits —
    // an empty map cannot become non-empty downstream. Skip the second git walk.
    if update_map.is_empty() {
        args.format.print("No updates found");
        return Ok(());
    }

    let ignore_is_empty = ctx.config.ignore.is_empty();
    let mut project_finders = ctx.project_finders;
    let all_finders = if ignore_is_empty {
        None
    } else {
        let mut all_finders = get_finders();

        // Reuse the ThreadSafeRepository already discovered by CommandContext::new
        // instead of re-running `gix::discover` per invocation. `all_config` clears
        // only the `ignore` filter (so nothing is filtered out here); the spread
        // moves `ctx.config` — not read after this point — instead of cloning it.
        //
        // Use the discovery-only `discover_project_dirs`: this second walk exists
        // solely to materialize the full unfiltered project set for
        // `apply_reverse_dependencies` / `merge_workspace_inherited_updates` /
        // `collect_workspace_projects`, which read only paths/names/deps/versions.
        // `is_changed` is never read from `all_finders`, so paying for the
        // base-branch diff + worktree-status change detection here would be pure
        // waste — skip it.
        let all_config = changepacks_core::Config {
            ignore: Vec::new(),
            ..ctx.config
        };
        discover_project_dirs(&ctx.repo, &mut all_finders, &all_config).await?;
        Some(all_finders)
    };

    // Apply reverse dependency updates across all discovered projects. When
    // `ignore` is non-empty, `all_finders` holds the full unfiltered tree;
    // otherwise the already-unfiltered `project_finders` IS that full set. Both
    // former branches ran the identical apply + merge over the same project
    // slice — only the finder source differed — so select the source once.
    let all_projects = collect_projects(all_finders.as_deref().unwrap_or(&project_finders));
    apply_reverse_dependencies(&mut update_map, &all_projects, &ctx.repo_root_path);

    // Merge workspace-inherited package updates into workspace entries. The
    // returned (member, workspace-root) pairs let the language-filtered
    // `applied_paths` snapshot below clear a folded member's changepack log in
    // lock-step with its workspace root.
    let merged_pairs =
        merge_workspace_inherited_updates(&mut update_map, &all_projects, &ctx.repo_root_path);

    // Filter update_map by language if specified.
    //
    // Prebuild `path_to_language` once so `retain` does O(1) lookups instead
    // of the previous O(N×M) any() closure that re-computed a `PathBuf` per
    // (map entry × project) pair — dropping allocations from `M × N` to
    // `N` (one PathBuf per project) plus `M` HashMap lookups.
    let language_filter_active = !args.language.is_empty();
    if language_filter_active {
        // Preallocate: `HashMap::from_iter` (via `collect`) does NOT use
        // `size_hint` to reserve capacity (unlike `Vec`), so on a
        // language-filtered `changepacks update -l rust` against a large
        // monorepo the map hits geometric-doubling reallocations. Summing
        // `finder.projects().len()` yields a tight upper bound (the
        // `filter_map` below can only shrink it when a project path lies
        // outside the repo root). Matches the preallocation policy already
        // applied in `sort_by_dep.rs` and `find_project_dirs.rs`.
        let cap: usize = total_project_count(&project_finders);
        let mut path_to_language: HashMap<&Path, Language> = HashMap::with_capacity(cap);
        for project in project_finders.iter().flat_map(|finder| finder.projects()) {
            if let Ok(rel) = get_relative_path_ref(&ctx.repo_root_path, project.path()) {
                path_to_language.insert(rel, project.language());
            }
        }
        // Filter through the shared `language_slice_contains` predicate — the
        // same match rule `retain_by_language` uses in
        // `options/language_options.rs`, so the "does this language match the
        // `--language` selection" rule has one definition across both filter
        // shells. Byte-identical filtering (short-circuits on the first match,
        // iterates `args.language` in order).
        update_map.retain(|path, _| {
            path_to_language
                .get(path.as_path())
                .is_some_and(|lang| language_slice_contains(&args.language, *lang))
        });
    }

    // The --language filter can empty the map (e.g. `update -l dart` with only
    // Rust logs pending); mirror the unfiltered empty case above instead of
    // printing an "Updates found:" banner over nothing and prompting.
    if update_map.is_empty() {
        args.format.print("No updates found");
        return Ok(());
    }

    if let FormatOptions::Stdout = args.format {
        println!("Updates found:");
    }

    // Snapshot applied paths before gen_changepack_result_map drains update_map.
    //
    // A workspace-inherited member folded by `merge_workspace_inherited_updates`
    // is no longer a key in `update_map` (its bump is owned by the workspace
    // root, whose path IS a key). Without re-adding the member path here,
    // `clear_applied_update_logs` would retain the member's changepack log and
    // re-apply it (double-bump) on the next `update`. Re-add each folded member
    // path IFF its workspace root actually survived the language filter (its
    // path is still in the applied set), so logs clear in lock-step with the
    // bump that satisfied them.
    let applied_paths = language_filter_active.then(|| {
        let mut set = HashSet::with_capacity(update_map.len() + merged_pairs.len());
        set.extend(update_map.keys().cloned());
        for (pkg_path, ws_path) in &merged_pairs {
            if set.contains(ws_path.as_path()) {
                set.insert(pkg_path.clone());
            }
        }
        set
    });

    let json_output = if let FormatOptions::Json = args.format {
        let update_types: Vec<_> = update_map
            .iter()
            .map(|(path, (update_type, _))| (path.clone(), *update_type))
            .collect();
        let output = serde_json::to_string_pretty(&gen_changepack_result_map(
            collect_projects(&project_finders).as_slice(),
            &ctx.repo_root_path,
            &mut update_map,
        )?)?;
        update_map.extend(
            update_types
                .into_iter()
                .map(|(path, update_type)| (path, (update_type, Vec::new()))),
        );
        Some(output)
    } else {
        None
    };

    let mut update_projects =
        collect_update_project_muts(&mut project_finders, &update_map, &ctx.repo_root_path)?;

    if !preview_and_confirm(args, prompter, &update_projects)? {
        return Ok(());
    }

    if let Some(all_finders) = all_finders.as_ref() {
        let workspace_projects = collect_workspace_projects(all_finders);

        apply_updates(&mut update_projects, &workspace_projects).await?;
        drop(update_projects);
    } else {
        apply_project_version_updates(&mut update_projects).await?;
        drop(update_projects);

        // Collect workspace projects after the mutable borrow is released
        let workspace_projects = collect_workspace_projects(&project_finders);
        if !workspace_projects.is_empty() {
            let update_projects =
                collect_update_project_refs(&project_finders, &update_map, &ctx.repo_root_path)?;
            let projects = packages_of(
                update_projects.len(),
                update_projects.iter().map(|(p, _)| *p),
            );

            apply_workspace_dependency_updates(&workspace_projects, &projects).await?;
        }
    }

    if let Some(json_output) = json_output {
        println!("{json_output}");
    }

    // Clear files
    match applied_paths {
        Some(applied) => clear_applied_update_logs(&ctx.changepacks_dir, &applied).await?,
        None => clear_update_logs(&ctx.changepacks_dir).await?,
    }

    Ok(())
}

/// Extract packages from projects, filtering to `Project::Package` variants.
///
/// Preallocates to `len` (the filter only drops `Project::Workspace`
/// variants — typically 0-1 entries in a package-heavy update). Matches the
/// preallocation policy applied throughout `sort_by_dep.rs`, `gen_update_map.rs`,
/// and `apply_reverse_dependencies`.
fn packages_of<'a>(
    len: usize,
    projects: impl IntoIterator<Item = &'a Project>,
) -> Vec<&'a dyn Package> {
    let mut packages: Vec<&'a dyn Package> = Vec::with_capacity(len);
    for project in projects {
        if let Project::Package(package) = project {
            packages.push(package.as_ref());
        }
    }
    packages
}

/// Display the pending updates, then apply the dry-run / confirmation gate.
///
/// Returns `Ok(false)` when the caller must stop without applying anything —
/// either `--dry-run` was set or the user declined the confirmation. In both
/// cases the user-facing message is already printed here, so the caller simply
/// returns `Ok(())` and prints nothing more. Returns `Ok(true)` to proceed.
///
/// Extracted to share the identical preview/dry-run/confirm sequence between the
/// two `handle_update_with_prompter` branches. Takes the `&mut Project`-pair
/// slice directly and reborrows each project as shared (`&**project`) inside the
/// display loop, so callers need no intermediate shared-reference vec.
///
/// Excluded from coverage: shares the interactive `prompter.confirm(...)` and
/// stdout display loop of its sole caller `handle_update_with_prompter`, itself
/// coverage-excluded for the same reason.
#[cfg(not(tarpaulin_include))]
fn preview_and_confirm(
    args: &UpdateArgs,
    prompter: &dyn Prompter,
    projects: &[UpdateProjectMut<'_>],
) -> Result<bool> {
    if let FormatOptions::Stdout = args.format {
        for (project, update_type) in projects {
            println!(
                "{} {}",
                &**project,
                display_update(project.version(), *update_type)?
            );
        }
    }

    if args.dry_run {
        args.format.print("Dry run, no updates will be made");
        return Ok(false);
    }

    // confirm
    let confirm = if args.yes {
        true
    } else {
        prompter.confirm("Are you sure you want to update the projects?")?
    };

    if !confirm {
        args.format.print("Update cancelled");
        return Ok(false);
    }

    Ok(true)
}

/// Shared body of the two `collect_update_project_*` collectors below, which
/// are byte-identical except for the finder accessor: `projects_mut()` yields
/// `&mut Project`, `projects()` yields `&Project`. That mutability difference
/// cannot be abstracted by plain generics, so — matching the repo's "same
/// body, different accessor" macro idiom (cf. the core crate's
/// `impl_projects_hashmap_accessors!` / `impl_basic_accessors!`) — a file-local
/// `macro_rules!` collapses the duplication. Preallocates to `update_map.len()`
/// (the loop pushes at most one entry per project) and relies on the enclosing
/// fn's `-> Result<...>` for the `?` on `get_relative_path_ref`.
macro_rules! collect_update_projects {
    ($finders:expr, $update_map:expr, $repo_root_path:expr, $accessor:ident) => {{
        let mut update_projects = Vec::with_capacity($update_map.len());

        for finder in $finders {
            for project in finder.$accessor() {
                if let Some((update_type, _)) =
                    $update_map.get(get_relative_path_ref($repo_root_path, project.path())?)
                {
                    update_projects.push((project, *update_type));
                }
            }
        }

        update_projects
    }};
}

#[cfg(not(tarpaulin_include))]
fn collect_update_project_muts<'a>(
    project_finders: &'a mut [Box<dyn ProjectFinder>],
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    repo_root_path: &Path,
) -> Result<Vec<UpdateProjectMut<'a>>> {
    let mut update_projects =
        collect_update_projects!(project_finders, update_map, repo_root_path, projects_mut);
    // Sorted order is user-visible: it drives the preview loop in
    // `preview_and_confirm`.
    update_projects.sort();
    Ok(update_projects)
}

#[cfg(not(tarpaulin_include))]
fn collect_update_project_refs<'a>(
    project_finders: &'a [Box<dyn ProjectFinder>],
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    repo_root_path: &Path,
) -> Result<Vec<UpdateProjectRef<'a>>> {
    // Unsorted: sole consumer is `packages_of` → `apply_workspace_dependency_updates`,
    // which looks up packages by name; order is irrelevant.
    Ok(collect_update_projects!(
        project_finders,
        update_map,
        repo_root_path,
        projects
    ))
}

#[cfg(not(tarpaulin_include))]
fn collect_workspace_projects<'a>(finders: &'a [Box<dyn ProjectFinder>]) -> Vec<WorkspaceRef<'a>> {
    let mut workspace_projects = Vec::new();

    for finder in finders {
        for project in finder.projects() {
            if let Project::Workspace(workspace) = project {
                workspace_projects.push(workspace.as_ref());
            }
        }
    }

    workspace_projects
}

async fn apply_updates(
    update_projects: &mut [UpdateProjectMut<'_>],
    workspace_projects: &[WorkspaceRef<'_>],
) -> Result<()> {
    apply_project_version_updates(update_projects).await?;

    // Fast-path the dominant no-op case: a package-only repo has zero
    // workspaces, so the `Vec<&dyn Package>` build + walk below and the
    // trailing `join_all` over `workspace_projects` are pure no-ops. Bail
    // here to skip that allocation and walk entirely. Behavior-preserving:
    // an empty `workspace_projects` already produced an empty `join_all`,
    // and `projects` is consumed only by that call. Semantic mirror of the
    // `is_empty()` guards in `apply_reverse_dependencies`,
    // `apply_update_on_rules`, and `RustWorkspace::update_workspace_dependencies`.
    if workspace_projects.is_empty() {
        return Ok(());
    }

    let projects = packages_of(
        update_projects.len(),
        update_projects.iter().map(|(p, _)| &**p),
    );

    apply_workspace_dependency_updates(workspace_projects, &projects).await?;

    Ok(())
}

async fn apply_project_version_updates(update_projects: &mut [UpdateProjectMut<'_>]) -> Result<()> {
    futures::future::join_all(
        update_projects
            .iter_mut()
            .map(|(project, update_type)| project.update_version(*update_type)),
    )
    .await
    .into_iter()
    .collect::<Result<()>>()
}

async fn apply_workspace_dependency_updates(
    workspace_projects: &[WorkspaceRef<'_>],
    projects: &[&dyn Package],
) -> Result<()> {
    futures::future::join_all(
        workspace_projects
            .iter()
            .map(|workspace| workspace.update_workspace_dependencies(projects)),
    )
    .await
    .into_iter()
    .collect::<Result<()>>()
}

/// Merge workspace-inherited package updates into workspace entries.
/// Packages with `version.workspace = true` should have their bumps promoted
/// to the workspace level (most significant bump wins). The packages are then
/// removed from the update map since their Cargo.toml doesn't need changes.
///
/// Returns the `(pkg_rel_path, ws_rel_path)` pairs that were actually folded
/// (member entry present and removed). The caller needs these to clear a folded
/// member's changepack log in lock-step with its workspace root: the member's
/// own path vanishes from `update_map` here, so a language-filtered
/// `applied_paths` snapshot must re-add it whenever its workspace root survived
/// the filter — otherwise `clear_applied_update_logs` retains the member's log
/// and it is re-applied (double-bumped) on the next `update`.
fn merge_workspace_inherited_updates(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    projects: &[&Project],
    repo_root_path: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    // Collect (pkg_rel_path, ws_rel_path) pairs to merge.
    // Preallocate: the loop below pushes AT MOST one entry per project
    // in the slice, so `projects.len()` is a tight upper bound that
    // avoids `Vec`'s geometric-doubling reallocations on vespera-shaped
    // monorepos with many workspace-inheriting members. Matches the
    // preallocation policy already applied throughout `sort_by_dep.rs`,
    // `find_project_dirs`, and the sibling `apply_reverse_dependencies`.
    let mut merge_targets: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(projects.len());

    for &project in projects {
        if let Project::Package(pkg) = project
            && pkg.inherits_workspace_version()
            && let Ok(rel_path) = get_relative_path_ref(repo_root_path, pkg.path())
            && update_map.contains_key(rel_path)
            && let Some(ws_root) = pkg.workspace_root_path()
            && let Ok(ws_rel_path) = get_relative_path(repo_root_path, ws_root)
        {
            merge_targets.push((rel_path.to_path_buf(), ws_rel_path));
        }
    }

    // Return the pairs actually folded so the caller can clear each member's
    // changepack log alongside its workspace root. `merge_targets.len()` is a
    // tight upper bound (each fold pushes at most one pair), matching the
    // preallocation policy applied throughout this file.
    let mut merged_pairs: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(merge_targets.len());
    for (pkg_path, ws_path) in merge_targets {
        // Remove takes ownership, avoiding Clone requirement
        if let Some((update_type, logs)) = update_map.remove(&pkg_path) {
            // Fast-path: `HashMap::get_mut` on an existing key is zero-alloc,
            // whereas `entry(ws_path.clone()).or_insert(...)` unconditionally
            // clones the `PathBuf` even when the workspace root is already a
            // key — common when several workspace-inheriting members fold into
            // the same root, or the root was already directly bumped. Mirrors
            // the established idiom in `gen_update_map.rs` and
            // `apply_reverse_dependencies`; semantics are byte-identical (same
            // mutable entry for the same `ws_path`).
            let ws_entry = if let Some(existing) = update_map.get_mut(&ws_path) {
                existing
            } else {
                update_map
                    .entry(ws_path.clone())
                    .or_insert((UpdateType::Patch, vec![]))
            };
            // More significant bump wins (Major=0 < Minor=1 < Patch=2)
            if update_type < ws_entry.0 {
                ws_entry.0 = update_type;
            }
            ws_entry.1.extend(logs);
            merged_pairs.push((pkg_path, ws_path));
        }
    }
    merged_pairs
}

#[cfg(test)]
mod tests {
    use super::{UpdateArgs, collect_projects, merge_workspace_inherited_updates};
    use anyhow::Result;
    use async_trait::async_trait;
    use changepacks_core::{
        ChangePackResultLog, Language, Package, Project, ProjectFinder, UpdateType,
    };
    use clap::Parser;
    use rstest::rstest;
    use std::{
        collections::{HashMap, HashSet},
        path::{Path, PathBuf},
    };

    use crate::options::FormatOptions;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        update: UpdateArgs,
    }

    // Field name `is_changed` matches the `impl_basic_accessors!()`
    // macro contract (see `crates/core/src/project_finder.rs`) so the
    // shared macro can generate every trivial accessor. Locks the
    // macro's field-name contract at this CLI-test surface the same way
    // `MockPackageForCheck` / `MockWorkspaceForCheck` in `check.rs` and
    // the core-crate test mocks do — a future rename of the macro's
    // expected field name trips a compile error here immediately.
    // Extra fields (`inherits_ws_version`, `workspace_root`) stay put
    // — the macro is scoped to the seven basic accessors and leaves the
    // Rust-specific `inherits_workspace_version` /
    // `workspace_root_path` overrides hand-rolled below.
    #[derive(Debug)]
    struct MockInheritPackage {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        dependencies: HashSet<String>,
        is_changed: bool,
        inherits_ws_version: bool,
        workspace_root: Option<PathBuf>,
    }

    impl MockInheritPackage {
        fn new(
            path: &str,
            relative_path: &str,
            inherits_ws_version: bool,
            workspace_root: Option<&str>,
        ) -> Self {
            Self {
                name: Some("mock-package".to_string()),
                version: Some("1.0.0".to_string()),
                path: PathBuf::from(path),
                relative_path: PathBuf::from(relative_path),
                language: Language::Rust,
                dependencies: HashSet::new(),
                is_changed: false,
                inherits_ws_version,
                workspace_root: workspace_root.map(PathBuf::from),
            }
        }
    }

    #[async_trait]
    impl Package for MockInheritPackage {
        // Consumes the same `impl_basic_accessors!()` macro that every
        // core-crate test mock uses — collapses the seven byte-identical
        // trivial accessors (`name`, `version`, `path`, `relative_path`,
        // `is_changed`, `set_changed`, `set_name`) into one macro
        // invocation. The two Rust-specific overrides
        // (`inherits_workspace_version`, `workspace_root_path`) stay
        // hand-rolled below because the macro is scoped to the seven
        // basic accessors.
        changepacks_core::impl_basic_accessors!();

        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            Ok(())
        }

        fn language(&self) -> Language {
            self.language
        }

        changepacks_core::impl_dependencies_accessors!();

        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
        }

        fn inherits_workspace_version(&self) -> bool {
            self.inherits_ws_version
        }

        fn workspace_root_path(&self) -> Option<&Path> {
            self.workspace_root.as_deref()
        }
    }

    #[derive(Debug)]
    struct MockFinder {
        projects: Vec<Project>,
    }

    impl MockFinder {
        fn new(projects: Vec<Project>) -> Self {
            Self { projects }
        }
    }

    #[async_trait]
    impl ProjectFinder for MockFinder {
        fn projects(&self) -> Vec<&Project> {
            self.projects.iter().collect()
        }

        fn projects_mut(&mut self) -> Vec<&mut Project> {
            self.projects.iter_mut().collect()
        }

        fn project_files(&self) -> &[&str] {
            &["Cargo.toml"]
        }

        async fn visit(&mut self, _path: &Path, _relative_path: &Path) -> Result<()> {
            Ok(())
        }
    }

    fn mock_package_project(
        path: &str,
        relative_path: &str,
        inherits_ws_version: bool,
        workspace_root: Option<&str>,
    ) -> Project {
        Project::Package(Box::new(MockInheritPackage::new(
            path,
            relative_path,
            inherits_ws_version,
            workspace_root,
        )))
    }

    fn mock_log(note: &str) -> ChangePackResultLog {
        ChangePackResultLog::new(UpdateType::Patch, note.to_string())
    }

    fn summarize_update_map(
        update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    ) -> HashMap<PathBuf, (UpdateType, usize)> {
        update_map
            .iter()
            .map(|(path, (update_type, logs))| (path.clone(), (*update_type, logs.len())))
            .collect()
    }

    #[test]
    fn test_merge_workspace_inherited_updates_no_inherited_packages() {
        let repo_root = Path::new("/repo");
        let pkg_rel_path = PathBuf::from("crates/foo/Cargo.toml");
        let mut update_map = HashMap::from([(
            pkg_rel_path.clone(),
            (UpdateType::Minor, vec![mock_log("pkg update")]),
        )]);

        let project_finders: Vec<Box<dyn ProjectFinder>> =
            vec![Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                false,
                Some("/repo/Cargo.toml"),
            )]))];

        let before = summarize_update_map(&update_map);
        merge_workspace_inherited_updates(
            &mut update_map,
            &collect_projects(&project_finders),
            repo_root,
        );

        assert_eq!(summarize_update_map(&update_map), before);
        assert!(update_map.contains_key(&pkg_rel_path));
    }

    #[test]
    fn test_merge_workspace_inherited_updates_basic_merge() {
        let repo_root = Path::new("/repo");
        let pkg_rel_path = PathBuf::from("crates/foo/Cargo.toml");
        let ws_rel_path = PathBuf::from("Cargo.toml");
        let mut update_map = HashMap::from([(
            pkg_rel_path.clone(),
            (UpdateType::Minor, vec![mock_log("pkg update")]),
        )]);

        let project_finders: Vec<Box<dyn ProjectFinder>> =
            vec![Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                true,
                Some("/repo/Cargo.toml"),
            )]))];

        merge_workspace_inherited_updates(
            &mut update_map,
            &collect_projects(&project_finders),
            repo_root,
        );

        assert!(!update_map.contains_key(&pkg_rel_path));
        let (update_type, logs) = update_map
            .get(&ws_rel_path)
            .expect("workspace entry should exist");
        assert_eq!(*update_type, UpdateType::Minor);
        assert_eq!(logs.len(), 1);
    }

    #[test]
    fn test_merge_workspace_inherited_updates_most_significant_bump_wins() {
        let repo_root = Path::new("/repo");
        let pkg1_rel_path = PathBuf::from("crates/foo/Cargo.toml");
        let pkg2_rel_path = PathBuf::from("crates/bar/Cargo.toml");
        let ws_rel_path = PathBuf::from("Cargo.toml");
        let mut update_map = HashMap::from([
            (
                pkg1_rel_path.clone(),
                (UpdateType::Minor, vec![mock_log("foo update")]),
            ),
            (
                pkg2_rel_path.clone(),
                (UpdateType::Major, vec![mock_log("bar update")]),
            ),
        ]);

        let project_finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(MockFinder::new(vec![
            mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                true,
                Some("/repo/Cargo.toml"),
            ),
            mock_package_project(
                "/repo/crates/bar/Cargo.toml",
                "crates/bar/Cargo.toml",
                true,
                Some("/repo/Cargo.toml"),
            ),
        ]))];

        merge_workspace_inherited_updates(
            &mut update_map,
            &collect_projects(&project_finders),
            repo_root,
        );

        assert!(!update_map.contains_key(&pkg1_rel_path));
        assert!(!update_map.contains_key(&pkg2_rel_path));
        let (update_type, logs) = update_map
            .get(&ws_rel_path)
            .expect("workspace entry should exist");
        assert_eq!(*update_type, UpdateType::Major);
        assert_eq!(logs.len(), 2);
    }

    #[test]
    fn test_merge_workspace_inherited_updates_package_not_in_update_map() {
        let repo_root = Path::new("/repo");
        let mut update_map = HashMap::from([(
            PathBuf::from("crates/bar/Cargo.toml"),
            (UpdateType::Patch, vec![mock_log("bar update")]),
        )]);

        let project_finders: Vec<Box<dyn ProjectFinder>> =
            vec![Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                true,
                Some("/repo/Cargo.toml"),
            )]))];

        let before = summarize_update_map(&update_map);
        merge_workspace_inherited_updates(
            &mut update_map,
            &collect_projects(&project_finders),
            repo_root,
        );

        assert_eq!(summarize_update_map(&update_map), before);
        assert!(!update_map.contains_key(&PathBuf::from("Cargo.toml")));
    }

    #[test]
    fn test_merge_workspace_inherited_updates_workspace_already_in_update_map() {
        let repo_root = Path::new("/repo");
        let pkg_rel_path = PathBuf::from("crates/foo/Cargo.toml");
        let ws_rel_path = PathBuf::from("Cargo.toml");
        let mut update_map = HashMap::from([
            (
                pkg_rel_path.clone(),
                (UpdateType::Major, vec![mock_log("foo update")]),
            ),
            (
                ws_rel_path.clone(),
                (UpdateType::Minor, vec![mock_log("workspace update")]),
            ),
        ]);

        let project_finders: Vec<Box<dyn ProjectFinder>> =
            vec![Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                true,
                Some("/repo/Cargo.toml"),
            )]))];

        merge_workspace_inherited_updates(
            &mut update_map,
            &collect_projects(&project_finders),
            repo_root,
        );

        assert!(!update_map.contains_key(&pkg_rel_path));
        let (update_type, logs) = update_map
            .get(&ws_rel_path)
            .expect("workspace entry should exist");
        assert_eq!(*update_type, UpdateType::Major);
        assert_eq!(logs.len(), 2);
    }

    #[test]
    fn test_merge_workspace_inherited_updates_logs_accumulated() {
        let repo_root = Path::new("/repo");
        let pkg1_rel_path = PathBuf::from("crates/foo/Cargo.toml");
        let pkg2_rel_path = PathBuf::from("crates/bar/Cargo.toml");
        let ws_rel_path = PathBuf::from("Cargo.toml");
        let mut update_map = HashMap::from([
            (
                pkg1_rel_path.clone(),
                (
                    UpdateType::Patch,
                    vec![mock_log("foo update 1"), mock_log("foo update 2")],
                ),
            ),
            (
                pkg2_rel_path.clone(),
                (UpdateType::Patch, vec![mock_log("bar update")]),
            ),
        ]);

        let project_finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(MockFinder::new(vec![
            mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                true,
                Some("/repo/Cargo.toml"),
            ),
            mock_package_project(
                "/repo/crates/bar/Cargo.toml",
                "crates/bar/Cargo.toml",
                true,
                Some("/repo/Cargo.toml"),
            ),
        ]))];

        merge_workspace_inherited_updates(
            &mut update_map,
            &collect_projects(&project_finders),
            repo_root,
        );

        assert!(!update_map.contains_key(&pkg1_rel_path));
        assert!(!update_map.contains_key(&pkg2_rel_path));
        let (update_type, logs) = update_map
            .get(&ws_rel_path)
            .expect("workspace entry should exist");
        assert_eq!(*update_type, UpdateType::Patch);
        assert_eq!(logs.len(), 3);
    }

    #[test]
    fn test_update_args_default() {
        let cli = TestCli::parse_from(["test"]);
        assert!(!cli.update.dry_run);
        assert!(!cli.update.yes);
        assert!(matches!(cli.update.format, FormatOptions::Stdout));
        assert!(!cli.update.remote);
    }

    // `--dry-run` (long) and `-d` (short) both flip the `dry_run` flag.
    #[rstest]
    #[case(&["test", "--dry-run"])]
    #[case(&["test", "-d"])]
    fn test_update_args_dry_run_flag(#[case] args: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert!(cli.update.dry_run);
    }

    // `--yes` (long) and `-y` (short) both flip the `yes` flag.
    #[rstest]
    #[case(&["test", "--yes"])]
    #[case(&["test", "-y"])]
    fn test_update_args_yes_flag(#[case] args: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert!(cli.update.yes);
    }

    #[test]
    fn test_update_args_with_format_json() {
        let cli = TestCli::parse_from(["test", "--format", "json"]);
        assert!(matches!(cli.update.format, FormatOptions::Json));
    }

    // `--remote` (long) and `-r` (short) both flip the `remote` flag.
    #[rstest]
    #[case(&["test", "--remote"])]
    #[case(&["test", "-r"])]
    fn test_update_args_remote_flag(#[case] args: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert!(cli.update.remote);
    }

    #[test]
    fn test_update_args_combined() {
        let cli =
            TestCli::parse_from(["test", "--dry-run", "--yes", "--format", "json", "--remote"]);
        assert!(cli.update.dry_run);
        assert!(cli.update.yes);
        assert!(matches!(cli.update.format, FormatOptions::Json));
        assert!(cli.update.remote);
    }

    // All three short flags together must set all three booleans; distinct
    // from the individual `*_flag` tests above because it also guards
    // against clap flag-conflict regressions.
    #[test]
    fn test_update_args_all_short_flags() {
        let cli = TestCli::parse_from(["test", "-d", "-y", "-r"]);
        assert!(cli.update.dry_run);
        assert!(cli.update.yes);
        assert!(cli.update.remote);
    }

    // `--language` / `-l` accumulate into `Vec<CliLanguage>`; the parsed
    // length must match the number of flags supplied.
    #[rstest]
    #[case(&["test", "--language", "node"], 1)]
    #[case(&["test", "-l", "rust"], 1)]
    #[case(&["test", "--language", "node", "--language", "python"], 2)]
    fn test_update_args_language_flag(#[case] args: &[&str], #[case] expected_len: usize) {
        let cli = TestCli::parse_from(args);
        assert_eq!(cli.update.language.len(), expected_len);
    }
}
