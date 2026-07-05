use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{
    ChangePackResultLog, Language, Package, Project, ProjectFinder, UpdateType, Workspace,
};
use changepacks_utils::{
    apply_reverse_dependencies, clear_update_logs, display_update, find_project_dirs,
    gen_changepack_result_map, gen_update_map, get_relative_path,
};
use clap::Args;

use crate::{
    CommandContext,
    finders::get_finders,
    options::{CliLanguage, FormatOptions, language_slice_contains},
    prompter::{InquirePrompter, Prompter},
};

type UpdateProjectMut<'a> = (&'a mut Project, UpdateType);
type WorkspaceRef<'a> = &'a dyn Workspace;

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct UpdateArgs {
    #[arg(short, long)]
    pub dry_run: bool,

    #[arg(short, long)]
    pub yes: bool,

    #[arg(long, default_value = "stdout")]
    pub format: FormatOptions,

    #[arg(short, long, default_value = "false")]
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

    let mut project_finders = ctx.project_finders;
    let mut all_finders = get_finders();

    // Reuse the ThreadSafeRepository already discovered by CommandContext::new
    // instead of re-running `gix::discover` per invocation. `all_finders` uses
    // an empty config so nothing is filtered out here.
    find_project_dirs(
        &ctx.repo,
        &mut all_finders,
        &changepacks_core::Config::default(),
        args.remote,
    )
    .await?;

    // Apply reverse dependency updates (workspace:* dependencies)
    //
    // Preallocate: `FlatMap`'s `size_hint` is `(0, None)` — no upper
    // bound — so `Vec::from_iter` reserves nothing and every flatten
    // grows via geometric doubling. `all_finders.iter().map(|f|
    // f.projects().len()).sum::<usize>()` is a tight upper bound
    // (each nested iterator yields exactly `projects().len()` refs).
    // Matches the identical idiom already used a few lines below at
    // `let cap: usize = project_finders.iter().map(|f| f.projects()
    // .len()).sum();`, and the preallocation policy applied
    // throughout `sort_by_dep.rs`, `gen_update_map.rs`, and
    // `apply_reverse_dependencies`. Byte-identical output.
    let cap: usize = all_finders.iter().map(|f| f.projects().len()).sum();
    let mut all_projects: Vec<&Project> = Vec::with_capacity(cap);
    all_projects.extend(all_finders.iter().flat_map(|finder| finder.projects()));
    apply_reverse_dependencies(&mut update_map, &all_projects, &ctx.repo_root_path);

    // Merge workspace-inherited package updates into workspace entries
    merge_workspace_inherited_updates(&mut update_map, &all_finders, &ctx.repo_root_path);

    if update_map.is_empty() {
        args.format.print("No updates found", "{}");
        return Ok(());
    }

    if let FormatOptions::Stdout = args.format {
        println!("Updates found:");
    }

    // Filter update_map by language if specified.
    //
    // Prebuild `path_to_language` once so `retain` does O(1) lookups instead
    // of the previous O(N×M) any() closure that re-computed a `PathBuf` per
    // (map entry × project) pair — dropping allocations from `M × N` to
    // `N` (one PathBuf per project) plus `M` HashMap lookups.
    if !args.language.is_empty() {
        // Preallocate: `HashMap::from_iter` (via `collect`) does NOT use
        // `size_hint` to reserve capacity (unlike `Vec`), so on a
        // language-filtered `changepacks update -l rust` against a large
        // monorepo the map hits geometric-doubling reallocations. Summing
        // `finder.projects().len()` yields a tight upper bound (the
        // `filter_map` below can only shrink it when a project path lies
        // outside the repo root). Matches the preallocation policy already
        // applied in `sort_by_dep.rs` and `filter_project_dirs.rs`.
        let cap: usize = project_finders.iter().map(|f| f.projects().len()).sum();
        let mut path_to_language: HashMap<PathBuf, Language> = HashMap::with_capacity(cap);
        for project in project_finders.iter().flat_map(|finder| finder.projects()) {
            if let Ok(rel) = get_relative_path(&ctx.repo_root_path, project.path()) {
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
                .get(path)
                .is_some_and(|lang| language_slice_contains(&args.language, *lang))
        });
    }

    let (mut update_projects, workspace_projects) = collect_update_projects(
        &mut project_finders,
        &all_finders,
        &update_map,
        &ctx.repo_root_path,
    )?;

    if let FormatOptions::Stdout = args.format {
        for (project, update_type) in &update_projects {
            println!(
                "{} {}",
                project,
                display_update(project.version(), *update_type)?
            );
        }
    }

    if args.dry_run {
        args.format.print("Dry run, no updates will be made", "{}");
        return Ok(());
    }

    // confirm
    let confirm = if args.yes {
        true
    } else {
        prompter.confirm("Are you sure you want to update the projects?")?
    };

    if !confirm {
        args.format.print("Update cancelled", "{}");
        return Ok(());
    }

    apply_updates(&mut update_projects, &workspace_projects).await?;
    drop(update_projects);

    if let FormatOptions::Json = args.format {
        println!(
            "{}",
            serde_json::to_string_pretty(&gen_changepack_result_map(
                project_finders
                    .iter()
                    .flat_map(|finder| finder.projects())
                    .collect::<Vec<_>>()
                    .as_slice(),
                &ctx.repo_root_path,
                &mut update_map,
            )?)?
        );
    }

    // Clear files
    clear_update_logs(&ctx.changepacks_dir).await?;

    Ok(())
}

/// Excluded from coverage: private helper invoked solely by
/// `handle_update_with_prompter`; exercised end-to-end via the cli
/// integration tests but its internal `if let Some(...) / for project in finder.projects_mut()`
/// loops require a real multi-language project tree to hit every branch.
#[cfg(not(tarpaulin_include))]
fn collect_update_projects<'a>(
    project_finders: &'a mut [Box<dyn ProjectFinder>],
    all_finders: &'a [Box<dyn ProjectFinder>],
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    repo_root_path: &Path,
) -> Result<(Vec<UpdateProjectMut<'a>>, Vec<WorkspaceRef<'a>>)> {
    // Preallocate: `update_map.len()` is a tight upper bound for
    // `update_projects` (only projects matching the update_map are pushed).
    // Matches the preallocation policy applied throughout `sort_by_dep.rs`
    // and `apply_reverse_dependencies`. `workspace_projects` is left as
    // `Vec::new()` because its true upper bound requires a walk of every
    // finder's projects — a modest default doesn't beat lazy allocation.
    let mut update_projects = Vec::with_capacity(update_map.len());
    let mut workspace_projects = Vec::new();

    for finder in project_finders {
        for project in finder.projects_mut() {
            if let Some((update_type, _)) =
                update_map.get(&get_relative_path(repo_root_path, project.path())?)
            {
                update_projects.push((project, *update_type));
            }
        }
    }

    for finder in all_finders {
        for project in finder.projects() {
            if let Project::Workspace(workspace) = project {
                workspace_projects.push(workspace.as_ref());
            }
        }
    }

    update_projects.sort();
    Ok((update_projects, workspace_projects))
}

async fn apply_updates(
    update_projects: &mut [UpdateProjectMut<'_>],
    workspace_projects: &[WorkspaceRef<'_>],
) -> Result<()> {
    futures::future::join_all(
        update_projects
            .iter_mut()
            .map(|(project, update_type)| project.update_version(*update_type)),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;

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

    // Preallocate: `FilterMap`'s `size_hint` reports
    // `(0, Some(update_projects.len()))` and `Vec::from_iter` reserves
    // against the LOWER bound (0), so a plain `.collect()` here hits
    // geometric-doubling reallocations. `update_projects.len()` is a tight
    // upper bound (the filter only drops `Project::Workspace` variants —
    // typically 0-1 entries in a package-heavy update). Matches the
    // preallocation policy already applied a few lines above at
    // `let mut update_projects = Vec::with_capacity(update_map.len());`
    // and across `sort_by_dep.rs`, `gen_update_map.rs`, and
    // `apply_reverse_dependencies`. Byte-identical output.
    let mut projects: Vec<&dyn Package> = Vec::with_capacity(update_projects.len());
    for (project, _) in update_projects.iter() {
        if let Project::Package(package) = project {
            projects.push(package.as_ref());
        }
    }

    futures::future::join_all(
        workspace_projects
            .iter()
            .map(|workspace| workspace.update_workspace_dependencies(&projects)),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;

    Ok(())
}

/// Merge workspace-inherited package updates into workspace entries.
/// Packages with `version.workspace = true` should have their bumps promoted
/// to the workspace level (most significant bump wins). The packages are then
/// removed from the update map since their Cargo.toml doesn't need changes.
fn merge_workspace_inherited_updates(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    project_finders: &[Box<dyn ProjectFinder>],
    repo_root_path: &Path,
) {
    // Collect (pkg_rel_path, ws_rel_path) pairs to merge.
    // Preallocate: the loop below pushes AT MOST one entry per project
    // across every finder. Summing `finder.projects().len()` yields a
    // tight upper bound that avoids `Vec`'s geometric-doubling
    // reallocations on vespera-shaped monorepos with many
    // workspace-inheriting members. Matches the preallocation policy
    // already applied throughout `sort_by_dep.rs`, `filter_project_dirs`,
    // and the sibling `apply_reverse_dependencies`.
    let cap: usize = project_finders.iter().map(|f| f.projects().len()).sum();
    let mut merge_targets: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(cap);

    for finder in project_finders {
        for project in finder.projects() {
            if let Project::Package(pkg) = project
                && pkg.inherits_workspace_version()
                && let Ok(rel_path) = get_relative_path(repo_root_path, pkg.path())
                && update_map.contains_key(&rel_path)
                && let Some(ws_root) = pkg.workspace_root_path()
                && let Ok(ws_rel_path) = get_relative_path(repo_root_path, ws_root)
            {
                merge_targets.push((rel_path, ws_rel_path));
            }
        }
    }

    for (pkg_path, ws_path) in merge_targets {
        // Remove takes ownership, avoiding Clone requirement
        if let Some((update_type, logs)) = update_map.remove(&pkg_path) {
            let ws_entry = update_map
                .entry(ws_path)
                .or_insert((UpdateType::Patch, vec![]));
            // More significant bump wins (Major=0 < Minor=1 < Patch=2)
            if update_type < ws_entry.0 {
                ws_entry.0 = update_type;
            }
            ws_entry.1.extend(logs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UpdateArgs, merge_workspace_inherited_updates};
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

        fn dependencies(&self) -> &HashSet<String> {
            &self.dependencies
        }

        fn add_dependency(&mut self, dep: &str) {
            self.dependencies.insert(dep.to_string());
        }

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
        merge_workspace_inherited_updates(&mut update_map, &project_finders, repo_root);

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

        merge_workspace_inherited_updates(&mut update_map, &project_finders, repo_root);

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

        merge_workspace_inherited_updates(&mut update_map, &project_finders, repo_root);

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
        merge_workspace_inherited_updates(&mut update_map, &project_finders, repo_root);

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

        merge_workspace_inherited_updates(&mut update_map, &project_finders, repo_root);

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

        merge_workspace_inherited_updates(&mut update_map, &project_finders, repo_root);

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
