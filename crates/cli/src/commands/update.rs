use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use changepacks_core::{
    ChangePackLog, ChangePackResultLog, Language, Package, Project, ProjectFinder, UpdateType,
    Workspace,
};
use changepacks_utils::{
    CARRY_FORWARD_LOG_PREFIX, clear_applied_update_logs, clear_update_logs,
    collect_changepack_log_paths, display_update, gen_update_map, get_relative_path,
    get_relative_path_ref,
};
use clap::Args;

use crate::{
    CommandContext,
    commands::{changepack_result_json, join_display, writeln_stdout},
    finders::collect_projects,
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
pub async fn handle_update_with_prompter(args: &UpdateArgs, prompter: &dyn Prompter) -> Result<()> {
    let ctx = CommandContext::new(args.remote).await?;
    let mut update_map = gen_update_map(&ctx.changepacks_dir, &ctx.config).await?;

    // Early return if no updates: `apply_update_on_rules` already ran inside
    // `gen_update_map`; `apply_reverse_dependencies` has an `is_empty()` fast-path;
    // `merge_workspace_inherited_updates` only mutates on `contains_key` hits —
    // an empty map cannot become non-empty downstream. Skip project collection
    // and dependency-graph processing.
    if update_map.is_empty() {
        args.format.print("No updates found")?;
        return Ok(());
    }

    let mut project_finders = ctx.project_finders;

    // Ignore boundaries apply to graph expansion as well as direct mutation:
    // dependents excluded during CommandContext discovery must not be scheduled.
    let projects = collect_projects(&project_finders);
    update_map.apply_reverse_dependencies(&projects, &ctx.repo_root_path)?;

    // Merge workspace-inherited package updates into workspace entries. The
    // returned (member, workspace-root) pairs let the language-filtered
    // `applied_paths` snapshot below clear a folded member's changepack log in
    // lock-step with its workspace root.
    let merged_pairs =
        merge_workspace_inherited_updates(&mut update_map, &projects, &ctx.repo_root_path);
    update_map.merge_provenance(&merged_pairs);

    // Filter update_map by language if specified.
    //
    // Prebuild `path_to_language` once so `retain` does O(1) lookups instead
    // of the previous O(N×M) any() closure that re-computed a `PathBuf` per
    // (map entry × project) pair — dropping allocations from `M × N` to
    // `N` (one PathBuf per project) plus `M` HashMap lookups.
    let language_filter_active = !args.language.is_empty();
    let carry_forward_logs = if language_filter_active {
        // Preallocate: `HashMap::from_iter` (via `collect`) does NOT use
        // `size_hint` to reserve capacity (unlike `Vec`), so on a
        // language-filtered `changepacks update -l rust` against a large
        // monorepo the map hits geometric-doubling reallocations.
        // `projects.len()` yields a tight upper bound (the loop below can only
        // shrink it when a project path lies outside the repo root). Matches
        // the preallocation policy already applied in `sort_by_dep.rs` and
        // `find_project_dirs.rs`.
        //
        // Iterates the `projects` slice collected above instead of re-running
        // `flat_map(|finder| finder.projects())`, which rebuilt one throwaway
        // `Vec<&Project>` per finder. `projects` is `collect_projects(&project_finders)`
        // over the same, still-unmutated finders, so the visited set and its
        // order are identical.
        let mut path_to_language: HashMap<&Path, Language> = HashMap::with_capacity(projects.len());
        for project in &projects {
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
        update_map.retain_updates(|path| {
            path_to_language
                .get(path)
                .is_some_and(|lang| language_slice_contains(&args.language, *lang))
        })
    } else {
        Vec::new()
    };

    // The --language filter can empty the map (e.g. `update -l dart` with only
    // Rust logs pending); mirror the unfiltered empty case above instead of
    // printing an "Updates found:" banner over nothing and prompting.
    if update_map.is_empty() {
        args.format.print("No updates found")?;
        return Ok(());
    }

    if let FormatOptions::Stdout = args.format {
        writeln_stdout(format_args!("Updates found:"))?;
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
    //
    // The set BORROWS its keys (`HashSet<&Path>`): `clear_applied_update_logs`
    // only probes membership with `&Path`, and both sources outlive that call —
    // `update_map` is not mutated after `retain_updates` above and is last read
    // when the transaction runs, `merged_pairs` is never mutated after it is
    // built. So the owned copies this used to make (one `PathBuf` clone per
    // update-map key plus one per folded workspace member) are pure waste.
    let applied_paths = language_filter_active.then(|| {
        let mut set: HashSet<&Path> = HashSet::with_capacity(update_map.len() + merged_pairs.len());
        set.extend(update_map.keys().map(PathBuf::as_path));
        for (pkg_path, ws_path) in &merged_pairs {
            if set.contains(ws_path.as_path()) {
                set.insert(pkg_path.as_path());
            }
        }
        set
    });

    // In --dry-run mode, preview_and_confirm returns Ok(false) and the handler returns
    // early (line ~202-204), so json_output is never printed. Skip the expensive
    // gen_changepack_result_map walk and serde_json::to_string_pretty serialization.
    let json_output = if !args.dry_run && matches!(args.format, FormatOptions::Json) {
        let output = changepack_result_json(projects.as_slice(), &ctx.repo_root_path, &update_map)?;
        Some(output)
    } else {
        None
    };

    // Capture filtered workspace paths before taking the mutable project borrow
    // so the transaction snapshots every in-scope manifest before the first write.
    //
    // Filters the `projects` slice collected above instead of calling
    // `collect_workspace_projects(&project_finders)`, which re-ran
    // `collect_projects` over all six finders to rebuild a second full
    // `Vec<&Project>` plus a `Vec<WorkspaceRef>` just to reach the workspace
    // subset already present here. Order is byte-identical:
    // `collect_workspace_projects` walks the same `collect_projects` output in
    // the same finder-then-`projects()` order and keeps only the `Workspace`
    // arm. This is also the last use of `projects`, so its immutable borrow of
    // `project_finders` ends here, before the mutable borrow taken below.
    let workspace_manifest_paths = projects
        .iter()
        .filter_map(|project| match project {
            Project::Workspace(workspace) => Some(workspace.path().to_path_buf()),
            Project::Package(_) => None,
        })
        .collect::<Vec<_>>();

    let mut update_projects =
        collect_update_project_muts(&mut project_finders, &update_map, &ctx.repo_root_path)?;
    validate_update_project_paths(&update_map, &update_projects, &ctx.repo_root_path)?;

    if !preview_and_confirm(args, prompter, &update_projects)? {
        return Ok(());
    }

    let manifest_paths = collect_update_snapshot_paths(&update_projects, workspace_manifest_paths);

    let snapshots = snapshot_update_state(manifest_paths, &ctx.changepacks_dir).await?;
    let project_result = apply_project_version_updates(&mut update_projects).await;
    drop(update_projects);

    // Every write past the snapshot belongs to one all-or-nothing transaction:
    // the first failure must skip the remaining steps and hand the error to
    // `rollback_update_error` below. Expressing that as one `async` block lets
    // `?` do the short-circuiting, instead of three nested `match`es whose
    // `Err` arms were pure `Err(error) => Err(error)` forwarding. Step order is
    // unchanged: version writes, then workspace dependency rewrites, then
    // carry-forward logs, then log cleanup.
    let transaction_result: Result<()> = async {
        project_result?;

        // Collect filtered workspace projects after the mutable borrow is released.
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

        persist_carry_forward_logs(&ctx.changepacks_dir, &carry_forward_logs).await?;

        match applied_paths {
            Some(applied) => clear_applied_update_logs(&ctx.changepacks_dir, &applied).await,
            None => clear_update_logs(&ctx.changepacks_dir).await,
        }
    }
    .await;
    if let Err(error) = transaction_result {
        return rollback_update_error(&snapshots, error).await;
    }

    if let Some(json_output) = json_output {
        // Most-piped write of the command (`changepacks update --format json | jq`).
        writeln_stdout(format_args!("{json_output}"))?;
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
/// Keeps the preview, dry-run, and confirmation gate together. Takes the
/// `&mut Project`-pair slice directly and reborrows each project as shared
/// (`&**project`) inside the display loop, so callers need no intermediate
/// shared-reference vec.
fn preview_and_confirm(
    args: &UpdateArgs,
    prompter: &dyn Prompter,
    projects: &[UpdateProjectMut<'_>],
) -> Result<bool> {
    if let FormatOptions::Stdout = args.format {
        // Acquire the stdout lock once for the whole preview instead of
        // re-locking (and re-flushing) per project as `println!` does. The
        // explicit `writeln!` also surfaces a broken-pipe / full-disk write
        // failure as an `anyhow` error through the existing `Result<bool>`
        // signature, where `println!` would panic.
        use std::io::Write as _;

        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for (project, update_type) in projects {
            writeln!(
                out,
                "{} {}",
                **project,
                display_update(project.version(), *update_type)?
            )?;
        }
    }

    if args.dry_run {
        args.format.print("Dry run, no updates will be made")?;
        return Ok(false);
    }

    // confirm
    let confirm =
        prompter.confirm_unless(args.yes, "Are you sure you want to update the projects?")?;

    if !confirm {
        args.format.print("Update cancelled")?;
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

fn collect_update_project_muts<'a>(
    project_finders: &'a mut [Box<dyn ProjectFinder>],
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    repo_root_path: &Path,
) -> Result<Vec<UpdateProjectMut<'a>>> {
    let mut update_projects =
        collect_update_projects!(project_finders, update_map, repo_root_path, projects_mut);
    update_projects.sort();
    Ok(update_projects)
}

fn validate_update_project_paths(
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    update_projects: &[UpdateProjectMut<'_>],
    repo_root_path: &Path,
) -> Result<()> {
    let mut project_paths = HashSet::with_capacity(update_projects.len());
    for (project, _) in update_projects {
        project_paths.insert(get_relative_path_ref(repo_root_path, project.path())?);
    }

    let mut unresolved_paths: Vec<&Path> = update_map
        .keys()
        .map(PathBuf::as_path)
        .filter(|path| !project_paths.contains(path))
        .collect();
    unresolved_paths.sort_unstable();

    if unresolved_paths.is_empty() {
        return Ok(());
    }

    // `Path::display()` is a `Display` adapter, so `join_display` writes it
    // straight into the buffer and no per-element `to_string` is needed.
    let rendered_paths = join_display(unresolved_paths.iter().map(|path| path.display()), ", ");

    bail!("unresolved changepack update paths: {rendered_paths}")
}

fn collect_update_project_refs<'a>(
    project_finders: &'a [Box<dyn ProjectFinder>],
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    repo_root_path: &Path,
) -> Result<Vec<UpdateProjectRef<'a>>> {
    Ok(collect_update_projects!(
        project_finders,
        update_map,
        repo_root_path,
        projects
    ))
}

/// Collect every workspace root held by `finders`, in finder order and, within
/// a finder, in `projects()` order.
///
/// Merges through [`collect_projects`] — which drives
/// [`ProjectFinder::extend_projects`] into one pre-sized buffer — instead of
/// looping `finder.projects()` per finder. The per-finder shape allocated and
/// immediately dropped one intermediate `Vec<&Project>` for each of the six
/// language finders. The merged buffer is still presized to
/// `total_project_count(finders)` inside `collect_projects`.
///
/// The result buffer, however, is reserved against the number of
/// [`Project::Workspace`] entries rather than the merged project count: a
/// monorepo holds O(1) workspace roots among O(N) packages, so inheriting the
/// project-count hint over-reserved one never-written slot per package on
/// every `changepacks update`. Counting the workspaces first costs one cheap
/// pass over borrows already in cache and reserves exactly what the loop
/// writes.
///
/// `handle_update_with_prompter` calls this once per invocation, after the
/// mutable project borrow is released, where a fresh borrow of the finders is
/// genuinely required. The earlier `workspace_manifest_paths` capture filters
/// the already-collected `projects` vector directly instead of calling in
/// here.
fn collect_workspace_projects<'a>(finders: &'a [Box<dyn ProjectFinder>]) -> Vec<WorkspaceRef<'a>> {
    let projects = collect_projects(finders);
    let workspace_count = projects
        .iter()
        .filter(|project| matches!(project, Project::Workspace(_)))
        .count();
    let mut workspace_projects = Vec::with_capacity(workspace_count);

    for project in projects {
        if let Project::Workspace(workspace) = project {
            workspace_projects.push(workspace.as_ref());
        }
    }

    workspace_projects
}

/// Collect every path the update transaction must snapshot: one manifest per
/// updated project, the sibling `gradle.properties` for each Java project, and
/// the workspace manifests, sorted and deduplicated.
///
/// The reservation is exact, not an upper bound. Only `Language::Java`
/// projects push a second path, so the previous `update_projects.len() * 2`
/// formula over-reserved exactly one `PathBuf` slot per non-Java project —
/// never written, on the transaction path of every `changepacks update`.
/// Counting the Java entries once up front costs a single cheap pass over
/// borrows already in cache and reserves only what the common path writes,
/// the same policy `PublishLoopState` documents for its failure buffers in
/// `commands::publish`.
fn collect_update_snapshot_paths(
    update_projects: &[UpdateProjectMut<'_>],
    workspace_manifest_paths: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let java_count = update_projects
        .iter()
        .filter(|(project, _)| project.language() == Language::Java)
        .count();
    let mut paths =
        Vec::with_capacity(update_projects.len() + java_count + workspace_manifest_paths.len());
    for (project, _) in update_projects {
        let manifest_path = project.path().to_path_buf();
        if project.language() == Language::Java {
            paths.push(manifest_path.with_file_name("gradle.properties"));
        }
        paths.push(manifest_path);
    }
    paths.extend(workspace_manifest_paths);
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// Write every carry-forward changepack log, concurrently.
///
/// Serialization happens first, in log order, so a `serde_json` failure still
/// question-mark propagates exactly as before (and now aborts before any file
/// is created). Each filename is drawn from `nanoid`, so the target paths are
/// provably disjoint and the writes race-free; they are therefore driven
/// through [`join_all_results`], whose polling-every-future-to-completion
/// contract is what this transaction needs — the rollback restores every path
/// the fan-out could have touched, not only the ones that succeeded.
async fn persist_carry_forward_logs(changepacks_dir: &Path, logs: &[ChangePackLog]) -> Result<()> {
    let mut pending = Vec::with_capacity(logs.len());
    for log in logs {
        let path = changepacks_dir.join(format!(
            "{CARRY_FORWARD_LOG_PREFIX}{}.json",
            nanoid::nanoid!()
        ));
        let content = serde_json::to_string_pretty(log)?;
        pending.push((path, content));
    }

    join_all_results(pending.iter().map(|(path, content)| async move {
        tokio::fs::write(path, content)
            .await
            .with_context(|| format!("Failed to write changepack log {}", path.display()))
    }))
    .await
}

struct FileSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}

struct UpdateStateSnapshot {
    manifests: Vec<FileSnapshot>,
    changepacks_dir: PathBuf,
    logs: Vec<FileSnapshot>,
}

async fn rollback_update_error(
    snapshots: &UpdateStateSnapshot,
    update_error: anyhow::Error,
) -> Result<()> {
    match rollback_update_state(snapshots).await {
        Ok(()) => Err(update_error),
        Err(rollback_error) => Err(update_error.context(format!(
            "failed to restore update transaction after error: {rollback_error}"
        ))),
    }
}

/// Callers pass pre-canonicalized manifest paths.
async fn snapshot_update_state(
    manifest_paths: Vec<PathBuf>,
    changepacks_dir: &Path,
) -> Result<UpdateStateSnapshot> {
    // The manifest batch and the changepack-log batch touch disjoint paths and
    // have no data dependency, so overlap the two I/O batches instead of
    // finishing every manifest read before the `.changepacks` directory walk
    // even starts. The result-decoding loops below stay in their original
    // order, so a manifest failure is still the first error surfaced.
    let (manifest_reads, log_batch) = tokio::join!(
        futures::future::join_all(manifest_paths.iter().map(tokio::fs::read)),
        async {
            let log_paths = collect_changepack_log_paths(changepacks_dir).await?;
            let log_reads = futures::future::join_all(log_paths.iter().map(tokio::fs::read)).await;
            anyhow::Ok((log_paths, log_reads))
        }
    );

    let mut snapshots = Vec::with_capacity(manifest_paths.len());
    for (path, result) in manifest_paths.into_iter().zip(manifest_reads) {
        let bytes = match result {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to snapshot manifest {}", path.display()));
            }
        };
        snapshots.push(FileSnapshot { path, bytes });
    }

    let (log_paths, log_reads) = log_batch?;
    let mut logs = Vec::with_capacity(log_paths.len());
    for (path, result) in log_paths.into_iter().zip(log_reads) {
        let bytes = result
            .with_context(|| format!("failed to snapshot changepack log {}", path.display()))?;
        logs.push(FileSnapshot {
            path,
            bytes: Some(bytes),
        });
    }

    Ok(UpdateStateSnapshot {
        manifests: snapshots,
        changepacks_dir: changepacks_dir.to_path_buf(),
        logs,
    })
}

/// Restore every snapshot concurrently, appending one failure string per path
/// that could not be restored.
///
/// The snapshot paths within a single batch are unique (manifest paths are
/// deduplicated by [`collect_update_snapshot_paths`] and log paths come from a
/// directory listing), so no two futures touch the same file and the fan-out
/// stays race-free. Failures are collected by zipping the results back against
/// `snapshots`, which keeps the reported order (and therefore the joined error
/// message) byte-identical to a sequential restore.
async fn restore_file_snapshots(snapshots: &[FileSnapshot], failures: &mut Vec<String>) {
    let results = futures::future::join_all(snapshots.iter().map(|snapshot| async move {
        match &snapshot.bytes {
            Some(bytes) => tokio::fs::write(&snapshot.path, bytes).await,
            None => match tokio::fs::remove_file(&snapshot.path).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        }
    }))
    .await;

    for (snapshot, result) in snapshots.iter().zip(results) {
        if let Err(error) = result {
            failures.push(format!("{}: {error}", snapshot.path.display()));
        }
    }
}

async fn rollback_update_state(snapshots: &UpdateStateSnapshot) -> Result<()> {
    let mut failures = Vec::new();

    restore_file_snapshots(&snapshots.manifests, &mut failures).await;

    let original_logs = snapshots
        .logs
        .iter()
        .map(|snapshot| snapshot.path.as_path())
        .collect::<HashSet<_>>();
    match collect_changepack_log_paths(&snapshots.changepacks_dir).await {
        Ok(current_logs) => {
            // Logs created by the failed update are distinct directory entries,
            // so their removals can overlap. Failures are re-ordered back to the
            // listing order by zipping the results against `stale_logs`.
            let stale_logs = current_logs
                .into_iter()
                .filter(|path| !original_logs.contains(path.as_path()))
                .collect::<Vec<_>>();
            let removals =
                futures::future::join_all(stale_logs.iter().map(tokio::fs::remove_file)).await;
            for (path, result) in stale_logs.iter().zip(removals) {
                if let Err(error) = result
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    failures.push(format!("{}: {error}", path.display()));
                }
            }
        }
        Err(error) => failures.push(format!("{}: {error}", snapshots.changepacks_dir.display())),
    }
    restore_file_snapshots(&snapshots.logs, &mut failures).await;

    if failures.is_empty() {
        Ok(())
    } else {
        bail!(failures.join("; "))
    }
}

/// Drive every future in `futures` concurrently to completion and return the
/// first error, if any.
///
/// Every future is polled to completion even when an earlier one already
/// failed: `join_all` never short-circuits, so the failure of one manifest
/// write cannot leave a sibling write half-done and unaccounted for. The
/// rollback path relies on that: it restores the snapshot of every path the
/// fan-out could have touched, not only the ones that succeeded.
async fn join_all_results<F>(futures: impl IntoIterator<Item = F>) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    futures::future::join_all(futures)
        .await
        .into_iter()
        .collect::<Result<()>>()
}

async fn apply_project_version_updates(update_projects: &mut [UpdateProjectMut<'_>]) -> Result<()> {
    join_all_results(
        update_projects
            .iter_mut()
            .map(|(project, update_type)| project.update_version(*update_type)),
    )
    .await
}

async fn apply_workspace_dependency_updates(
    workspace_projects: &[WorkspaceRef<'_>],
    projects: &[&dyn Package],
) -> Result<()> {
    join_all_results(
        workspace_projects
            .iter()
            .map(|workspace| workspace.update_workspace_dependencies(projects)),
    )
    .await
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
    // Single pass: fold each workspace-inheriting member into its workspace
    // root as soon as it is discovered, instead of staging the candidate pairs
    // in an intermediate `Vec` and re-walking it. The split was never required
    // by the borrow checker (`rel_path` borrows from `pkg.path()`, not from
    // `update_map`), and fusing is observationally equivalent: every project
    // owns a distinct manifest path, and a `Project` is either a `Package` or a
    // `Workspace` but never both, so a member's `rel_path` can never collide
    // with a workspace root path inserted earlier in the same pass. Each
    // iteration therefore only ever touches keys no other iteration reads.
    // Fusing removes one `Vec` allocation and the redundant `contains_key`
    // probe that preceded every `remove`.
    //
    // The returned pairs are the folds that actually happened, so the caller can
    // clear each member's changepack log alongside its workspace root. Only a
    // `Project::Package` that inherits its workspace version can ever push, so
    // `projects.len()` was not a tight bound: in a polyglot monorepo the
    // workspaces, the standalone packages, and every package that pins its own
    // version are all counted by it and none of them can fold, leaving a
    // two-`PathBuf` slot reserved and never written for each. Counting the
    // inheriting members once up front is a cheap pass over borrows already in
    // cache and reserves the real upper bound, the same exact-reservation
    // policy `collect_update_snapshot_paths` already applies with its
    // `java_count`.
    let inheriting_count = projects
        .iter()
        .filter(
            |project| matches!(project, Project::Package(pkg) if pkg.inherits_workspace_version()),
        )
        .count();
    let mut merged_pairs: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(inheriting_count);

    for &project in projects {
        if let Project::Package(pkg) = project
            && pkg.inherits_workspace_version()
            && let Ok(rel_path) = get_relative_path_ref(repo_root_path, pkg.path())
            && let Some(ws_root) = pkg.workspace_root_path()
            && let Ok(ws_path) = get_relative_path(repo_root_path, ws_root)
            // `remove` doubles as the presence check: it takes ownership of the
            // member entry, avoiding a `Clone` requirement on the logs.
            && let Some((update_type, logs)) = update_map.remove(rel_path)
        {
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
            merged_pairs.push((rel_path.to_path_buf(), ws_path));
        }
    }
    merged_pairs
}

#[cfg(test)]
mod tests {
    use super::{
        UpdateArgs, UpdateProjectMut, WorkspaceRef, apply_project_version_updates,
        apply_workspace_dependency_updates, collect_projects, collect_update_project_muts,
        collect_update_project_refs, collect_update_snapshot_paths, collect_workspace_projects,
        merge_workspace_inherited_updates, packages_of, persist_carry_forward_logs,
        preview_and_confirm, rollback_update_error, snapshot_update_state,
        validate_update_project_paths,
    };
    use anyhow::{Result, bail};
    use async_trait::async_trait;
    use changepacks_core::{
        ChangePackLog, ChangePackResultLog, Language, Package, Project, ProjectFinder, UpdateType,
        Workspace,
    };
    use changepacks_utils::{
        clear_update_logs, collect_changepack_log_paths,
        test_support::{DirGuard, git_add_and_commit, init_git_repo},
    };
    use clap::Parser;
    use rstest::rstest;
    use serial_test::serial;
    use std::{
        collections::{BTreeMap, HashMap, HashSet},
        future::Future,
        path::{Path, PathBuf},
    };
    use tempfile::TempDir;

    use crate::{options::FormatOptions, prompter::MockPrompter};

    async fn apply_updates_unchecked(
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

    async fn run_update_transaction(
        manifest_paths: Vec<PathBuf>,
        changepacks_dir: &Path,
        update: impl Future<Output = Result<()>>,
        cleanup: impl Future<Output = Result<()>>,
    ) -> Result<()> {
        let snapshots = snapshot_update_state(manifest_paths, changepacks_dir).await?;
        let result = match update.await {
            Ok(()) => cleanup.await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => Ok(()),
            Err(update_error) => rollback_update_error(&snapshots, update_error).await,
        }
    }

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        update: UpdateArgs,
    }

    // Field name `is_changed` matches the `impl_basic_accessors!()`
    // macro contract (see `crates/core/src/project_finder.rs`) so the
    // shared macro can generate every trivial accessor. Locks the
    // macro's field-name contract at this CLI-test surface the same way
    // the core-crate test mocks `MockPackage` / `MockWorkspace` in
    // `crates/core/src/test_support.rs` do — a future rename of the
    // macro's expected field name trips a compile error here immediately.
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
    struct FileUpdatingPackage {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        dependencies: HashSet<String>,
        is_changed: bool,
        updated_bytes: Vec<u8>,
    }

    #[async_trait]
    impl Package for FileUpdatingPackage {
        changepacks_core::impl_basic_accessors!();

        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            tokio::fs::write(&self.path, &self.updated_bytes).await?;
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
    }

    #[derive(Debug)]
    struct FileUpdatingWorkspace {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        dependencies: HashSet<String>,
        is_changed: bool,
        updated_bytes: Vec<u8>,
        fail_dependency_update: bool,
    }

    #[async_trait]
    impl Workspace for FileUpdatingWorkspace {
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

        async fn update_workspace_dependencies(&self, _packages: &[&dyn Package]) -> Result<()> {
            tokio::fs::write(&self.path, &self.updated_bytes).await?;
            if self.fail_dependency_update {
                bail!("deliberate workspace dependency failure");
            }
            Ok(())
        }
    }

    fn file_updating_project(path: PathBuf, updated_bytes: &[u8]) -> Project {
        Project::Package(Box::new(FileUpdatingPackage {
            name: Some("file-package".to_string()),
            version: Some("1.0.0".to_string()),
            relative_path: PathBuf::from("package.json"),
            path,
            language: Language::Node,
            dependencies: HashSet::new(),
            is_changed: false,
            updated_bytes: updated_bytes.to_vec(),
        }))
    }

    fn file_updating_workspace(
        path: PathBuf,
        updated_bytes: &[u8],
        fail_dependency_update: bool,
    ) -> FileUpdatingWorkspace {
        FileUpdatingWorkspace {
            name: Some("file-workspace".to_string()),
            version: Some("1.0.0".to_string()),
            relative_path: PathBuf::from("Cargo.toml"),
            path,
            language: Language::Rust,
            dependencies: HashSet::new(),
            is_changed: false,
            updated_bytes: updated_bytes.to_vec(),
            fail_dependency_update,
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

        fn project_count(&self) -> usize {
            self.projects.len()
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

    fn update_args(dry_run: bool, yes: bool, format: FormatOptions) -> UpdateArgs {
        UpdateArgs {
            dry_run,
            yes,
            format,
            remote: false,
            language: Vec::new(),
        }
    }

    async fn ignored_update_fixture(
        ignore_pattern: &str,
        changed_manifest: &str,
        files: &[(&str, &str)],
    ) -> Result<TempDir> {
        let repository = TempDir::new()?;
        init_git_repo(repository.path());

        let changepacks_dir = repository.path().join(".changepacks");
        tokio::fs::create_dir_all(&changepacks_dir).await?;
        tokio::fs::write(
            changepacks_dir.join("config.json"),
            serde_json::to_vec(&serde_json::json!({ "ignore": [ignore_pattern] }))?,
        )
        .await?;

        for (relative_path, contents) in files {
            let path = repository.path().join(relative_path);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(path, contents).await?;
        }

        let changes = BTreeMap::from([(PathBuf::from(changed_manifest), UpdateType::Minor)]);
        let log = ChangePackLog::new(changes, "visible package update".to_string());
        tokio::fs::write(
            changepacks_dir.join("changepack_log_ignore_boundary.json"),
            serde_json::to_vec(&log)?,
        )
        .await?;
        git_add_and_commit(repository.path(), "ignored update fixture");

        Ok(repository)
    }

    fn assert_no_unresolved_ignored_path(result: &Result<()>, ignored_path: &str) {
        let unresolved_ignored_path = result.as_ref().err().is_some_and(|error| {
            let message = error.to_string();
            message.contains("unresolved changepack update paths") && message.contains(ignored_path)
        });
        assert!(
            !unresolved_ignored_path,
            "update reported ignored manifest as unresolved: {}",
            result
                .as_ref()
                .expect_err("an unresolved error was expected")
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_update_does_not_expand_into_ignored_dependent() -> Result<()> {
        const VISIBLE_MANIFEST: &str = r#"{
  "name": "visible-core",
  "version": "1.0.0"
}
"#;
        const IGNORED_MANIFEST: &str = r#"{
  "name": "ignored-dependent",
  "version": "1.0.0",
  "dependencies": {
    "visible-core": "workspace:*"
  }
}
"#;
        let repository = ignored_update_fixture(
            "packages/ignored/**",
            "packages/visible/package.json",
            &[
                ("packages/visible/package.json", VISIBLE_MANIFEST),
                ("packages/ignored/package.json", IGNORED_MANIFEST),
            ],
        )
        .await?;
        let visible_path = repository.path().join("packages/visible/package.json");
        let ignored_path = repository.path().join("packages/ignored/package.json");
        let _current_dir = DirGuard::change_to(repository.path());

        let result = super::handle_update_with_prompter(
            &update_args(false, true, FormatOptions::Json),
            &MockPrompter::default(),
        )
        .await;

        assert_no_unresolved_ignored_path(&result, "packages/ignored/package.json");
        result?;
        let visible: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(visible_path).await?)?;
        assert_eq!(visible["version"], "1.1.0");
        assert_eq!(
            tokio::fs::read(ignored_path).await?,
            IGNORED_MANIFEST.as_bytes()
        );
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn test_update_does_not_rewrite_ignored_rust_workspace_path_dependency() -> Result<()> {
        const VISIBLE_MANIFEST: &str = r#"[package]
name = "visible-core"
version = "1.0.0"
edition = "2024"
"#;
        const IGNORED_MANIFEST: &str = r#"[workspace]
members = []
resolver = "3"

[workspace.dependencies.visible-core]
version = "1.0.0"
path = "../visible"
"#;
        let repository = ignored_update_fixture(
            "ignored/**",
            "visible/Cargo.toml",
            &[
                ("visible/Cargo.toml", VISIBLE_MANIFEST),
                ("ignored/Cargo.toml", IGNORED_MANIFEST),
            ],
        )
        .await?;
        let visible_path = repository.path().join("visible/Cargo.toml");
        let ignored_path = repository.path().join("ignored/Cargo.toml");
        let _current_dir = DirGuard::change_to(repository.path());

        let result = super::handle_update_with_prompter(
            &update_args(false, true, FormatOptions::Json),
            &MockPrompter::default(),
        )
        .await;

        assert_no_unresolved_ignored_path(&result, "ignored/Cargo.toml");
        result?;
        let visible = tokio::fs::read_to_string(visible_path).await?;
        assert!(visible.contains("version = \"1.1.0\""));
        assert_eq!(
            tokio::fs::read(ignored_path).await?,
            IGNORED_MANIFEST.as_bytes()
        );
        Ok(())
    }

    #[test]
    fn test_preview_and_confirm_dry_run_stops_before_confirmation() -> Result<()> {
        let mut project = mock_package_project("/repo/Cargo.toml", "Cargo.toml", false, None);
        let projects = vec![(&mut project, UpdateType::Patch)];
        let prompter = MockPrompter {
            confirm_value: true,
            ..Default::default()
        };

        assert!(!preview_and_confirm(
            &update_args(true, false, FormatOptions::Json),
            &prompter,
            &projects,
        )?);
        Ok(())
    }

    #[test]
    fn test_preview_and_confirm_yes_bypasses_declining_prompter() -> Result<()> {
        let mut project = mock_package_project("/repo/Cargo.toml", "Cargo.toml", false, None);
        let projects = vec![(&mut project, UpdateType::Minor)];
        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };

        assert!(preview_and_confirm(
            &update_args(false, true, FormatOptions::Json),
            &prompter,
            &projects,
        )?);
        Ok(())
    }

    #[test]
    fn test_preview_and_confirm_decline_stops_update() -> Result<()> {
        let mut project = mock_package_project("/repo/Cargo.toml", "Cargo.toml", false, None);
        let projects = vec![(&mut project, UpdateType::Major)];
        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };

        assert!(!preview_and_confirm(
            &update_args(false, false, FormatOptions::Json),
            &prompter,
            &projects,
        )?);
        Ok(())
    }

    #[test]
    fn test_preview_and_confirm_propagates_preview_error() {
        let mut package = MockInheritPackage::new("/repo/Cargo.toml", "Cargo.toml", false, None);
        package.version = Some("not-a-version".to_string());
        let mut project = Project::Package(Box::new(package));
        let projects = vec![(&mut project, UpdateType::Patch)];

        let error = preview_and_confirm(
            &update_args(false, false, FormatOptions::Stdout),
            &MockPrompter::default(),
            &projects,
        )
        .expect_err("invalid version should fail while rendering the preview");

        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn test_collect_workspace_projects_returns_only_workspaces() {
        let workspace_path = PathBuf::from("/repo/Cargo.toml");
        let finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(MockFinder::new(vec![
            mock_package_project(
                "/repo/crates/pkg/Cargo.toml",
                "crates/pkg/Cargo.toml",
                false,
                None,
            ),
            Project::Workspace(Box::new(file_updating_workspace(
                workspace_path.clone(),
                b"updated",
                false,
            ))),
        ]))];

        let workspaces = collect_workspace_projects(&finders);

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].path(), workspace_path);
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
    fn test_collect_update_snapshot_paths_includes_deduplicated_java_properties() {
        let project_path = PathBuf::from("/repo/java/build.gradle.kts");
        let properties_path = PathBuf::from("/repo/java/gradle.properties");
        let mut project = Project::Package(Box::new(FileUpdatingPackage {
            name: Some("java-package".to_string()),
            version: Some("1.0.0".to_string()),
            path: project_path.clone(),
            relative_path: PathBuf::from("java/build.gradle.kts"),
            language: Language::Java,
            dependencies: HashSet::new(),
            is_changed: false,
            updated_bytes: Vec::new(),
        }));
        let update_projects = vec![(&mut project, UpdateType::Patch)];

        let paths = collect_update_snapshot_paths(
            &update_projects,
            vec![project_path.clone(), project_path.clone()],
        );

        assert_eq!(paths, vec![project_path, properties_path]);
    }

    #[tokio::test]
    async fn test_failed_update_transaction_restores_manifests_and_preserves_logs() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let package_path = temp_dir.path().join("package.json");
        let workspace_path = temp_dir.path().join("Cargo.toml");
        let changepacks_dir = temp_dir.path().join(".changepacks");
        let log_path = changepacks_dir.join("changepack_log_atomic.json");
        let original_package = b"{\r\n  \"version\": \"1.0.0\"\r\n}\r\n";
        let original_workspace = b"[workspace.package]\nversion = \"1.0.0\"\n";
        let original_log = b"{\"changes\":{\"package.json\":\"patch\"}}\n";
        tokio::fs::create_dir(&changepacks_dir).await?;
        tokio::fs::write(&package_path, original_package).await?;
        tokio::fs::write(&workspace_path, original_workspace).await?;
        tokio::fs::write(&log_path, original_log).await?;

        let mut project = file_updating_project(package_path.clone(), b"{\"version\":\"1.0.1\"}\n");
        let workspace = file_updating_workspace(
            workspace_path.clone(),
            b"[workspace.package]\nversion = \"1.0.1\"\n",
            true,
        );
        let mut update_projects = vec![(&mut project, UpdateType::Patch)];
        let workspaces: Vec<&dyn Workspace> = vec![&workspace];

        let error = run_update_transaction(
            vec![
                package_path.clone(),
                workspace_path.clone(),
                package_path.clone(),
            ],
            &changepacks_dir,
            apply_updates_unchecked(&mut update_projects, &workspaces),
            async { Ok(()) },
        )
        .await
        .expect_err("the workspace rewrite should fail after writing");

        assert_eq!(error.to_string(), "deliberate workspace dependency failure");
        assert_eq!(tokio::fs::read(&package_path).await?, original_package);
        assert_eq!(tokio::fs::read(&workspace_path).await?, original_workspace);
        assert_eq!(tokio::fs::read(&log_path).await?, original_log);
        Ok(())
    }

    #[tokio::test]
    async fn test_later_update_failure_restores_gradle_properties_and_log() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let properties_path = temp_dir.path().join("gradle.properties");
        let later_manifest_path = temp_dir.path().join("package.json");
        let changepacks_dir = temp_dir.path().join(".changepacks");
        let log_path = changepacks_dir.join("changepack_log_java.json");
        let original_properties = b"version = 1.0.0 # exact\r\n";
        let original_manifest = b"{\"version\":\"1.0.0\"}\n";
        let original_log = b"{\"changes\":{\"build.gradle.kts\":\"Patch\"}}\n";
        tokio::fs::create_dir(&changepacks_dir).await?;
        tokio::fs::write(&properties_path, original_properties).await?;
        tokio::fs::write(&later_manifest_path, original_manifest).await?;
        tokio::fs::write(&log_path, original_log).await?;

        let error = run_update_transaction(
            vec![properties_path.clone(), later_manifest_path.clone()],
            &changepacks_dir,
            async {
                tokio::fs::write(&properties_path, b"version = 1.0.1 # exact\r\n").await?;
                tokio::fs::write(&later_manifest_path, b"{\"version\":\"1.0.1\"}\n").await?;
                bail!("deliberate later project update failure")
            },
            async { Ok(()) },
        )
        .await
        .expect_err("a later project update should fail after the property write");

        assert_eq!(error.to_string(), "deliberate later project update failure");
        assert_eq!(
            tokio::fs::read(&properties_path).await?,
            original_properties
        );
        assert_eq!(
            tokio::fs::read(&later_manifest_path).await?,
            original_manifest
        );
        assert_eq!(tokio::fs::read(&log_path).await?, original_log);
        Ok(())
    }

    #[tokio::test]
    async fn test_failed_update_removes_new_gradle_properties_and_preserves_log() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let properties_path = temp_dir.path().join("gradle.properties");
        let changepacks_dir = temp_dir.path().join(".changepacks");
        let log_path = changepacks_dir.join("changepack_log_java.json");
        let original_log = b"{\"changes\":{\"build.gradle\":\"Patch\"}}\n";
        tokio::fs::create_dir(&changepacks_dir).await?;
        tokio::fs::write(&log_path, original_log).await?;

        let error = run_update_transaction(
            vec![properties_path.clone()],
            &changepacks_dir,
            async {
                tokio::fs::write(&properties_path, b"version=1.0.1\n").await?;
                bail!("deliberate failure after creating properties")
            },
            async { Ok(()) },
        )
        .await
        .expect_err("the update should fail after creating gradle.properties");

        assert_eq!(
            error.to_string(),
            "deliberate failure after creating properties"
        );
        assert!(!tokio::fs::try_exists(&properties_path).await?);
        assert_eq!(tokio::fs::read(&log_path).await?, original_log);
        Ok(())
    }

    #[tokio::test]
    async fn test_failed_update_transaction_removes_manifest_missing_at_snapshot() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let manifest_path = temp_dir.path().join("generated.json");
        let changepacks_dir = temp_dir.path().join(".changepacks");
        tokio::fs::create_dir(&changepacks_dir).await?;

        let error = run_update_transaction(
            vec![manifest_path.clone()],
            &changepacks_dir,
            async {
                tokio::fs::write(&manifest_path, b"generated during update\n").await?;
                bail!("deliberate update failure")
            },
            async { Ok(()) },
        )
        .await
        .expect_err("the update should fail after creating the manifest");

        assert_eq!(error.to_string(), "deliberate update failure");
        assert!(!manifest_path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_cleanup_failure_transaction_restores_multiple_original_logs_and_removes_new_log()
    -> Result<()> {
        let temp_dir = TempDir::new()?;
        let manifest_path = temp_dir.path().join("package.json");
        let changepacks_dir = temp_dir.path().join(".changepacks");
        let removed_log = changepacks_dir.join("changepack_log_removed.json");
        let rewritten_log = changepacks_dir.join("changepack_log_rewritten.JSON");
        let created_log = changepacks_dir.join("changepack_log_created.json");
        let original_manifest = b"{\r\n  \"version\": \"1.0.0\"\r\n}\r\n";
        let original_removed_log = b"{\"changes\":{\"package.json\":\"Patch\"}}\n";
        let original_rewritten_log = b"{\n  \"changes\": {\"other/package.json\": \"Minor\"}\n}\n";

        tokio::fs::create_dir(&changepacks_dir).await?;
        tokio::fs::write(&manifest_path, original_manifest).await?;
        tokio::fs::write(&removed_log, original_removed_log).await?;
        tokio::fs::write(&rewritten_log, original_rewritten_log).await?;

        let error = run_update_transaction(
            vec![manifest_path.clone()],
            &changepacks_dir,
            async {
                tokio::fs::write(&manifest_path, b"{\"version\":\"1.0.1\"}\n").await?;
                Ok(())
            },
            async {
                tokio::fs::remove_file(&removed_log).await?;
                tokio::fs::write(&rewritten_log, b"{\"changes\":{}}\n").await?;
                tokio::fs::write(&created_log, b"new log that did not exist before\n").await?;
                bail!("deliberate mid-cleanup failure")
            },
        )
        .await
        .expect_err("cleanup should fail after partially mutating the log set");

        assert_eq!(error.to_string(), "deliberate mid-cleanup failure");
        assert_eq!(tokio::fs::read(&manifest_path).await?, original_manifest);
        assert_eq!(tokio::fs::read(&removed_log).await?, original_removed_log);
        assert_eq!(
            tokio::fs::read(&rewritten_log).await?,
            original_rewritten_log
        );
        assert!(
            !created_log.exists(),
            "rollback must remove newly created logs"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_cleanup_failure_restores_gradle_properties_and_changepack_logs() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let properties_path = temp_dir.path().join("gradle.properties");
        let changepacks_dir = temp_dir.path().join(".changepacks");
        let log_path = changepacks_dir.join("changepack_log_java.json");
        let original_properties = b"version: 3.0.0 ! exact\n";
        let original_log = b"{\"changes\":{\"build.gradle\":\"Minor\"}}\n";
        tokio::fs::create_dir(&changepacks_dir).await?;
        tokio::fs::write(&properties_path, original_properties).await?;
        tokio::fs::write(&log_path, original_log).await?;

        let error = run_update_transaction(
            vec![properties_path.clone()],
            &changepacks_dir,
            async {
                tokio::fs::write(&properties_path, b"version: 3.1.0 ! exact\n").await?;
                Ok(())
            },
            async {
                tokio::fs::remove_file(&log_path).await?;
                bail!("deliberate cleanup failure after removing Java log")
            },
        )
        .await
        .expect_err("cleanup should fail after the Gradle property write");

        assert_eq!(
            error.to_string(),
            "deliberate cleanup failure after removing Java log"
        );
        assert_eq!(
            tokio::fs::read(&properties_path).await?,
            original_properties
        );
        assert_eq!(tokio::fs::read(&log_path).await?, original_log);
        Ok(())
    }

    #[tokio::test]
    async fn test_post_write_failure_restores_original_log_and_removes_carry_forward_log()
    -> Result<()> {
        let temp_dir = TempDir::new()?;
        let changepacks_dir = temp_dir.path().join(".changepacks");
        let original_log_path = changepacks_dir.join("changepack_log_original.json");
        let original_log = b"{\"changes\":{\"Cargo.toml\":\"Minor\"},\"note\":\"original\"}\n";
        tokio::fs::create_dir(&changepacks_dir).await?;
        tokio::fs::write(&original_log_path, original_log).await?;
        let carry_forward_log = ChangePackLog::new(
            BTreeMap::from([(PathBuf::from("bridge/node/package.json"), UpdateType::Patch)]),
            "generated bridge update".to_string(),
        );

        let error = run_update_transaction(Vec::new(), &changepacks_dir, async { Ok(()) }, async {
            persist_carry_forward_logs(&changepacks_dir, &[carry_forward_log]).await?;
            assert_eq!(
                collect_changepack_log_paths(&changepacks_dir).await?.len(),
                2
            );
            bail!("deliberate failure after carry-forward write")
        })
        .await
        .expect_err("the transaction should fail after writing the carry-forward log");

        assert_eq!(
            error.to_string(),
            "deliberate failure after carry-forward write"
        );
        assert_eq!(tokio::fs::read(&original_log_path).await?, original_log);
        assert_eq!(
            collect_changepack_log_paths(&changepacks_dir).await?,
            vec![original_log_path]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_successful_update_transaction_updates_manifests_then_clears_logs() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let package_path = temp_dir.path().join("package.json");
        let workspace_path = temp_dir.path().join("Cargo.toml");
        let changepacks_dir = temp_dir.path().join(".changepacks");
        let log_path = changepacks_dir.join("changepack_log_atomic.json");
        let updated_package = b"{\"version\":\"1.0.1\"}\n";
        let updated_workspace = b"[workspace.package]\nversion = \"1.0.1\"\n";
        tokio::fs::create_dir(&changepacks_dir).await?;
        tokio::fs::write(&package_path, b"{\"version\":\"1.0.0\"}\n").await?;
        tokio::fs::write(&workspace_path, b"version = \"1.0.0\"\n").await?;
        tokio::fs::write(&log_path, b"{}\n").await?;

        let mut project = file_updating_project(package_path.clone(), updated_package);
        let workspace = file_updating_workspace(workspace_path.clone(), updated_workspace, false);
        let mut update_projects = vec![(&mut project, UpdateType::Patch)];
        let workspaces: Vec<&dyn Workspace> = vec![&workspace];

        run_update_transaction(
            vec![package_path.clone(), workspace_path.clone()],
            &changepacks_dir,
            apply_updates_unchecked(&mut update_projects, &workspaces),
            clear_update_logs(&changepacks_dir),
        )
        .await?;

        assert_eq!(tokio::fs::read(&package_path).await?, updated_package);
        assert_eq!(tokio::fs::read(&workspace_path).await?, updated_workspace);
        assert!(!log_path.exists());
        Ok(())
    }

    #[test]
    fn test_collect_update_project_refs_matches_manifest_paths_in_finder_order() -> Result<()> {
        let repo_root = Path::new("/repo");
        let project_finders: Vec<Box<dyn ProjectFinder>> = vec![
            Box::new(MockFinder::new(vec![
                mock_package_project(
                    "/repo/crates/z/Cargo.toml",
                    "crates/z/Cargo.toml",
                    false,
                    None,
                ),
                mock_package_project(
                    "/repo/crates/ignored/Cargo.toml",
                    "crates/ignored/Cargo.toml",
                    false,
                    None,
                ),
            ])),
            Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/crates/a/Cargo.toml",
                "crates/a/Cargo.toml",
                false,
                None,
            )])),
        ];
        let update_map = HashMap::from([
            (
                PathBuf::from("crates/a/Cargo.toml"),
                (UpdateType::Minor, vec![mock_log("a update")]),
            ),
            (
                PathBuf::from("crates/z/Cargo.toml"),
                (UpdateType::Major, vec![mock_log("z update")]),
            ),
        ]);

        let update_projects =
            collect_update_project_refs(&project_finders, &update_map, repo_root)?;
        let actual = update_projects
            .iter()
            .map(|(project, update_type)| (project.relative_path(), *update_type))
            .collect::<Vec<_>>();

        assert_eq!(
            actual,
            vec![
                (Path::new("crates/z/Cargo.toml"), UpdateType::Major),
                (Path::new("crates/a/Cargo.toml"), UpdateType::Minor),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_validate_update_project_paths_accepts_complete_matches() -> Result<()> {
        let repo_root = Path::new("/repo");
        let mut project_finders: Vec<Box<dyn ProjectFinder>> =
            vec![Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/Cargo.toml",
                "Cargo.toml",
                false,
                None,
            )]))];
        let update_map = HashMap::from([(
            PathBuf::from("Cargo.toml"),
            (UpdateType::Minor, vec![mock_log("workspace update")]),
        )]);
        let update_projects =
            collect_update_project_muts(&mut project_finders, &update_map, repo_root)?;

        let result = validate_update_project_paths(&update_map, &update_projects, repo_root);

        assert!(result.is_ok());
        Ok(())
    }

    #[test]
    fn test_validate_update_project_paths_rejects_one_unresolved_key() -> Result<()> {
        let repo_root = Path::new("/repo");
        let mut project_finders: Vec<Box<dyn ProjectFinder>> =
            vec![Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                false,
                None,
            )]))];
        let update_map = HashMap::from([
            (
                PathBuf::from("crates/foo/Cargo.toml"),
                (UpdateType::Minor, vec![mock_log("foo update")]),
            ),
            (
                PathBuf::from("missing/Cargo.toml"),
                (UpdateType::Patch, vec![mock_log("missing update")]),
            ),
        ]);
        let update_projects =
            collect_update_project_muts(&mut project_finders, &update_map, repo_root)?;

        let error = validate_update_project_paths(&update_map, &update_projects, repo_root)
            .expect_err("the unmatched update path should be rejected");

        assert_eq!(
            error.to_string(),
            "unresolved changepack update paths: missing/Cargo.toml"
        );
        Ok(())
    }

    #[test]
    fn test_validate_update_project_paths_rejects_multiple_unresolved_keys() -> Result<()> {
        let repo_root = Path::new("/repo");
        let mut project_finders: Vec<Box<dyn ProjectFinder>> =
            vec![Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                false,
                None,
            )]))];
        // Seeded out of lexicographic order so the assertion below pins both the
        // `sort_unstable` ordering and the `, ` separator emitted between every
        // pair of rendered paths, including the middle element.
        let update_map = HashMap::from([
            (
                PathBuf::from("missing/zeta/Cargo.toml"),
                (UpdateType::Patch, vec![mock_log("zeta update")]),
            ),
            (
                PathBuf::from("crates/foo/Cargo.toml"),
                (UpdateType::Minor, vec![mock_log("foo update")]),
            ),
            (
                PathBuf::from("missing/alpha/Cargo.toml"),
                (UpdateType::Major, vec![mock_log("alpha update")]),
            ),
            (
                PathBuf::from("missing/mid/Cargo.toml"),
                (UpdateType::Patch, vec![mock_log("mid update")]),
            ),
        ]);
        let update_projects =
            collect_update_project_muts(&mut project_finders, &update_map, repo_root)?;

        let error = validate_update_project_paths(&update_map, &update_projects, repo_root)
            .expect_err("every unmatched update path should be rejected");

        assert_eq!(
            error.to_string(),
            "unresolved changepack update paths: missing/alpha/Cargo.toml, \
             missing/mid/Cargo.toml, missing/zeta/Cargo.toml"
        );
        Ok(())
    }

    #[test]
    fn test_validate_update_project_paths_preserves_language_filtered_logs() -> Result<()> {
        let repo_root = Path::new("/repo");
        let mut project_finders: Vec<Box<dyn ProjectFinder>> =
            vec![Box::new(MockFinder::new(vec![mock_package_project(
                "/repo/crates/foo/Cargo.toml",
                "crates/foo/Cargo.toml",
                false,
                None,
            )]))];
        let update_map = HashMap::from([
            (
                PathBuf::from("selected/z/Cargo.toml"),
                (UpdateType::Patch, vec![mock_log("selected z update")]),
            ),
            (
                PathBuf::from("crates/foo/Cargo.toml"),
                (UpdateType::Minor, vec![mock_log("selected foo update")]),
            ),
            (
                PathBuf::from("selected/a/Cargo.toml"),
                (UpdateType::Patch, vec![mock_log("selected a update")]),
            ),
        ]);
        let before = summarize_update_map(&update_map);
        let update_projects =
            collect_update_project_muts(&mut project_finders, &update_map, repo_root)?;

        let error = validate_update_project_paths(&update_map, &update_projects, repo_root)
            .expect_err("unresolved selected-language paths should block log clearing");

        assert_eq!(
            error.to_string(),
            "unresolved changepack update paths: selected/a/Cargo.toml, selected/z/Cargo.toml"
        );
        assert_eq!(summarize_update_map(&update_map), before);
        Ok(())
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
