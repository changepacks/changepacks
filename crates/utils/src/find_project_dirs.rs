use anyhow::{Context, Result};
use changepacks_core::{
    Config, Project, ProjectFinder, contains_changepacks_component, has_extension_ignore_ascii_case,
};
use gix::{ThreadSafeRepository, bstr::ByteSlice, features::progress};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// Resolve a git ref (local or remote) to the commit it ultimately names.
fn peel_to_commit_id(mut reference: gix::Reference<'_>) -> Result<gix::ObjectId> {
    Ok(reference.peel_to_commit()?.id)
}

/// Whether any entry of `project_files` claims `path` (whose file name is the
/// already-decoded `file_name`).
///
/// This is the ONLY place that understands both forms
/// [`ProjectFinder::project_files`] may return:
///
/// - a bare **file name** (`"package.json"`, `"Cargo.toml"`, ...), compared
///   byte-for-byte against `file_name`; and
/// - a leading-dot **extension** (`".csproj"`, returned only by
///   `CSharpProjectFinder`), compared case-insensitively against the path's
///   extension so `App.CSPROJ` is claimed alongside `App.csproj`.
///
/// The extension form exists solely for the dispatch decision here. The
/// defaulted [`ProjectFinder::matches_project_file`] implements only the
/// file-name form, so it can never match an extension entry — an
/// extension-based finder must therefore re-check the extension itself inside
/// `visit()` (see `CSharpProjectFinder::visit`), because being dispatched here
/// is not the same as being validated there.
fn project_files_can_visit_path(project_files: &[&str], path: &Path, file_name: &str) -> bool {
    project_files.iter().any(|project_file| {
        if *project_file == file_name {
            return true;
        }

        let Some(extension) = project_file.strip_prefix('.') else {
            return false;
        };

        has_extension_ignore_ascii_case(path, extension)
    })
}

fn build_config_gitignore(git_root_path: &Path, config: &Config) -> Result<Option<Gitignore>> {
    if config.ignore.is_empty() {
        return Ok(None);
    }

    let mut builder = GitignoreBuilder::new(git_root_path);
    for pattern in &config.ignore {
        builder
            .add_line(None, pattern)
            .with_context(|| format!("Invalid ignore pattern in config: {pattern}"))?;
    }

    Ok(Some(builder.build().context(
        "Failed to build ignore matcher from config patterns",
    )?))
}

fn config_ignores_path(gitignore: Option<&Gitignore>, path: &Path) -> bool {
    gitignore.is_some_and(|gitignore| gitignore.matched(path, false).is_ignore())
}

fn should_dispatch_change(gitignore: Option<&Gitignore>, path: &Path) -> bool {
    !contains_changepacks_component(path) && !config_ignores_path(gitignore, path)
}

fn collect_dispatchable_paths(
    paths: impl IntoIterator<Item = Result<PathBuf>>,
    gitignore: Option<&Gitignore>,
    error_context: &'static str,
) -> Result<Vec<PathBuf>> {
    paths
        .into_iter()
        .filter(|entry| match entry {
            Ok(path) => should_dispatch_change(gitignore, path),
            Err(_) => true,
        })
        .collect::<Result<Vec<_>>>()
        .context(error_context)
}

/// Discovery walk over an already-materialized thread-local repository.
///
/// Takes the `gix::Repository` by value and hands it back on success so
/// [`find_project_dirs`] can reuse the very same handle for its base-branch
/// diff / merge-base / worktree-status passes instead of paying for a second
/// `to_thread_local()`. Ownership (rather than `&gix::Repository`) is required
/// because the handle stays live across the `finder.visit(..).await` points and
/// `gix::Repository` is `Send` but not `Sync`, so a borrow would make this
/// future non-`Send` and break the FFI bridges that spawn it.
async fn discover_project_dirs_with_gitignore(
    repo: gix::Repository,
    project_finders: &mut [Box<dyn ProjectFinder>],
    git_root_path: &Path,
    gitignore: Option<&Gitignore>,
) -> Result<gix::Repository> {
    let index = repo
        .index()
        .context("Failed to get index, Please add files to git")?;
    // Iterate through git tracked files and find matching project files
    for entry in index.entries() {
        let file_path = entry.path(&index);
        // `BStr::to_str_lossy()` returns `Cow<'_, str>` that borrows on the
        // valid-UTF-8 happy path (every path in the git index is UTF-8 on
        // Windows and virtually every Unix repo) and only allocates on
        // invalid UTF-8 — matching the previous `Display`-based lossy
        // replacement behavior. The old `to_string()` unconditionally
        // allocated a fresh `String` per tracked file; on large monorepos
        // that is thousands of small heap allocations per invocation of
        // `find_project_dirs`, and every command (`check`, `update`,
        // `changepack`, `publish`) flows through this walk.
        let file_path_str = file_path.to_str_lossy();
        let path = Path::new(file_path_str.as_ref());

        // Check if this file matches any of the project files.
        //
        // `path` is ALREADY the git-relative path from the gix index entry, so
        // the previous `get_relative_path(git_root_path, git_root_path.join(path))`
        // trip was strip_prefix over the same prefix we just joined — a
        // guaranteed round-trip that allocated a fresh `PathBuf` per tracked
        // file. Pass `path` (which is `&Path`) directly to both call sites.

        // Skip if path matches ignore patterns (gitignore supports ! negation).
        // Do this BEFORE joining `abs_path`: on a monorepo whose config
        // discards most files (e.g. this repo's own
        // `["**/*", "!crates/changepacks/Cargo.toml", …]`), every tracked
        // file passes through here but only a handful survive the filter.
        // Joining after the guard skips one `PathBuf::join` allocation per
        // ignored file — byte-identical semantics because `abs_path` is only
        // computed (lazily) for the finder `visit` calls below.
        if config_ignores_path(gitignore, path) {
            continue;
        }

        // Dispatch to every finder that can visit this path in ONE sequential
        // pass. `abs_path` is computed lazily via `get_or_insert_with`, so the
        // `git_root_path.join(path)` allocation happens only once a finder
        // actually matches — never for files no finder claims. Each finder's
        // `project_files()` list is pairwise disjoint (at most one finder
        // matches a path), so a sequential `.await` per finder — rather than
        // `try_join_all` over a filtered set — preserves visit order with no
        // concurrency to gain, and the first `Err` still aborts the walk.
        //
        // Hoist file_name extraction before the finder loop: extract once per
        // git-index entry instead of once per finder per entry. With 6 finders,
        // this eliminates 5 redundant OsStr decodes per tracked file.
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        let mut abs_path: Option<PathBuf> = None;
        for finder in project_finders.iter_mut() {
            if !project_files_can_visit_path(finder.project_files(), path, file_name) {
                continue;
            }
            let abs = abs_path.get_or_insert_with(|| git_root_path.join(path));
            finder.visit(abs, path).await?;
        }
    }

    // Post-visit finalization (resolves deferred state like workspace-inherited versions)
    for finder in project_finders.iter_mut() {
        finder.finalize().await?;
    }

    // Fallback: set git repo name for projects with no name
    // Priority: remote origin repo name > directory name
    //
    // Collect `targets` FIRST — a cheap immutable walk over finders — and
    // defer the entire `repo_name` computation until we know at least one
    // no-name project actually needs it. On the dominant case (every
    // package.json / Cargo.toml / pyproject.toml carries a `name` field),
    // `targets` is empty and we skip the `try_find_remote("origin")` git
    // config walk + URL parsing chain entirely.
    //
    // `extend_projects_mut` drains each finder straight into the shared
    // buffer, so the throwaway `Vec<&mut Project>` that
    // `flat_map(|f| f.projects_mut())` allocated per finder — six per CLI run —
    // is never built. Order is unchanged (`HashMap::values_mut()` either way),
    // and the no-name filter simply moves from the iterator to a `retain` over
    // the merged buffer.
    let mut targets: Vec<&mut Project> = Vec::new();
    for finder in project_finders.iter_mut() {
        finder.extend_projects_mut(&mut targets);
    }
    targets.retain(|p| p.name().is_none());
    if !targets.is_empty() {
        let repo_name = repo
            .try_find_remote("origin")
            .and_then(|r| r.ok())
            .and_then(|remote| {
                let url = remote.url(gix::remote::Direction::Fetch)?;
                let path = url.path.to_str_lossy();
                let name = path.rsplit('/').next()?;
                let name = name.strip_suffix(".git").unwrap_or(name);
                if name.is_empty() {
                    None
                } else {
                    Some(name.to_string())
                }
            })
            .or_else(|| {
                git_root_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)
            });
        // Move the owned `String` into the LAST no-name project instead of
        // cloning it for every no-name project. On repos where multiple
        // projects lack an explicit name (Node workspaces without `"name"`
        // fields at the root), this saves one `String::clone` per invocation.
        // Behavior is byte-identical: every no-name project still ends up
        // with the same `repo_name` string; only the last one moves rather
        // than clones.
        if let Some(repo_name) = repo_name
            && let Some((last, rest)) = targets.split_last_mut()
        {
            for project in rest {
                project.set_name(repo_name.clone());
            }
            last.set_name(repo_name);
        }
    }

    Ok(repo)
}

/// Find project directories containing specific files from git tracked files
///
/// # Errors
/// Returns error if git operations fail, gitignore parsing fails, or project visiting fails.
///
pub async fn find_project_dirs(
    repo: &ThreadSafeRepository,
    project_finders: &mut [Box<dyn ProjectFinder>],
    config: &Config,
    remote: bool,
) -> Result<()> {
    let git_root_path = repo.work_dir().context("Not a working directory")?;
    let gitignore = build_config_gitignore(git_root_path, config)?;

    // Materialize the thread-local `gix::Repository` ONCE and reuse it for the
    // discovery walk, the base-branch diff, the merge base and the worktree
    // status pass below. `to_thread_local()` rebuilds a full `Repository`
    // (object store handle, ref store, config snapshot), so calling it once
    // per entry point instead of once per phase halves that setup cost on
    // every CLI command. The discovery walk hands the handle back so it can be
    // reused here without a second materialization.
    let repo = discover_project_dirs_with_gitignore(
        repo.to_thread_local(),
        project_finders,
        git_root_path,
        gitignore.as_ref(),
    )
    .await?;

    // diff from the merge base — compute FIRST so `diff.len()` can seed the
    // `unique_files` capacity below without an intermediate
    // `changed_files: Vec<PathBuf>` allocation for the status entries.
    let base_commit_id = if remote {
        peel_to_commit_id(
            repo.find_remote("origin")
                .context(
                    "Git remote 'origin' is not configured; --remote requires an 'origin' remote",
                )?
                .repo
                .find_reference(&format!("refs/remotes/origin/{}", config.base_branch))
                .with_context(|| {
                    format!(
                        "remote base branch 'origin/{}' not found. Did you fetch first?",
                        config.base_branch
                    )
                })?,
        )?
    } else {
        peel_to_commit_id(
            repo.find_reference(&format!("refs/heads/{}", config.base_branch))
                .with_context(|| {
                    format!(
                        "base branch '{}' not found in local refs",
                        config.base_branch
                    )
                })?,
        )?
    };
    let head_commit = repo.head_commit()?;
    let head_tree = head_commit.tree()?;
    let comparison_tree = repo
        .merge_base(base_commit_id, head_commit.id)?
        .object()?
        .try_into_commit()?
        .tree()?;
    let diff = collect_dispatchable_paths(
        repo.diff_tree_to_tree(
            Some(&head_tree),
            Some(&comparison_tree),
            gix::diff::Options::default(),
        )
        .context("Failed to enumerate changed tree")?
        .into_iter()
        // gix reports ancestor tree entries alongside their changed leaves.
        // Dispatch only changes with a leaf on either side so file-specific
        // ignore patterns cannot be bypassed by an unmatched parent directory.
        .filter(|change| {
            change.entry_mode().is_no_tree() || change.source_entry_mode_and_id().0.is_no_tree()
        })
        .map(|change| {
            Ok(change
                .location()
                .to_path()
                .context("Failed to convert changed tree path")?
                .to_path_buf())
        }),
        gitignore.as_ref(),
        "Failed to enumerate changed tree paths",
    )?;

    // Dedupe status ∪ diff before dispatching to `check_changed`.
    // The common case during a live edit against `main` is a file that has
    // both an uncommitted local edit AND appears in the base-branch diff, so
    // the previous `chain(...)` walked it twice — once per list — and each
    // walk fired `check_changed` on every finder. `check_changed` is
    // idempotent (its default guard returns early once `is_changed()` is
    // true), so behavior is preserved, but the duplicate walk still cost
    // `M` (finder count) `should_mark_changed` scans per repeated file.
    // Collecting into a `HashSet<PathBuf>` collapses that to exactly one
    // traversal per unique file. Also keeps the `git_root_path.join(file)`
    // hoist from a previous iteration intact.
    let status_paths = collect_dispatchable_paths(
        repo.status(progress::Discard)
            .context("Failed to prepare repository status")?
            .into_index_worktree_iter(Vec::new())
            .context("Failed to enumerate index and worktree changes")?
            .map(|entry| {
                let entry = entry.context("Failed to enumerate worktree status entry")?;
                Ok(entry
                    .rela_path()
                    .to_path()
                    .context("Failed to convert worktree status path")?
                    .to_path_buf())
            }),
        gitignore.as_ref(),
        "Failed to enumerate worktree status paths",
    )?;

    // Preallocate the exact upper bound now that both sources are materialized.
    // Cross-source duplicates can only leave unused capacity, while reserving
    // from `diff` alone can require the set to grow for worktree-only changes.
    let mut unique_files: HashSet<PathBuf> =
        HashSet::with_capacity(diff.len() + status_paths.len());
    unique_files.extend(status_paths);
    unique_files.extend(diff);

    // Resolve every unique changed file to an absolute path ONCE, then dispatch
    // the whole batch to each finder. The previous file-major nested loop
    // rebuilt a fresh `Vec<&mut Project>` (via `projects_mut()`) for every
    // (file, finder) pair — `F` files × `M` finders allocations. `check_changed_many`
    // takes one `projects_mut()` snapshot per finder, dropping that to `M`
    // Vec allocations total. Order-flip safety (project-major vs file-major) is
    // guaranteed by `Project::check_changed` monotonicity — see its doc comment
    // on `ProjectFinder::check_changed_many`.
    //
    // `into_iter` (not `iter`) so each source `PathBuf` is moved out and freed
    // as its absolute form is built, instead of keeping the whole set alive
    // alongside `abs_paths`; `unique_files` is not read after this point.
    let abs_paths: Vec<PathBuf> = unique_files
        .into_iter()
        .map(|file| git_root_path.join(file))
        .collect();
    for finder in project_finders.iter_mut() {
        finder.check_changed_many(&abs_paths)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{discover_repo, git_add_and_commit, init_git_repo, run_git};
    use changepacks_node::finder::NodeProjectFinder;
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    use tempfile::TempDir;
    use tokio::fs;

    /// Shared recording of every `check_changed_many` batch a finder observed.
    type ChangedBatches = Arc<Mutex<Vec<Vec<PathBuf>>>>;

    #[derive(Debug)]
    struct RecordingNodeFinder {
        inner: NodeProjectFinder,
        visits: Arc<AtomicUsize>,
        finalizations: Arc<AtomicUsize>,
        changed_batches: ChangedBatches,
    }

    impl RecordingNodeFinder {
        fn new(
            visits: Arc<AtomicUsize>,
            finalizations: Arc<AtomicUsize>,
            changed_batches: ChangedBatches,
        ) -> Self {
            Self {
                inner: NodeProjectFinder::new(),
                visits,
                finalizations,
                changed_batches,
            }
        }
    }

    impl ProjectFinder for RecordingNodeFinder {
        fn projects(&self) -> Vec<&Project> {
            self.inner.projects()
        }

        fn projects_mut(&mut self) -> Vec<&mut Project> {
            self.inner.projects_mut()
        }

        fn project_count(&self) -> usize {
            self.inner.project_count()
        }

        fn project_files(&self) -> &[&str] {
            self.inner.project_files()
        }

        fn visit<'life0, 'life1, 'life2, 'async_trait>(
            &'life0 mut self,
            path: &'life1 Path,
            relative_path: &'life2 Path,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            'life2: 'async_trait,
            Self: 'async_trait,
        {
            self.visits.fetch_add(1, Ordering::SeqCst);
            Box::pin(self.inner.visit(path, relative_path))
        }

        fn check_changed_many(&mut self, paths: &[PathBuf]) -> Result<()> {
            self.changed_batches.lock().unwrap().push(paths.to_vec());
            self.inner.check_changed_many(paths)
        }

        fn finalize<'life0, 'async_trait>(
            &'life0 mut self,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            self.finalizations.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    fn recording_node_finders() -> (Vec<Box<dyn ProjectFinder>>, ChangedBatches) {
        let changed_batches = Arc::new(Mutex::new(Vec::new()));
        let finder = RecordingNodeFinder::new(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::clone(&changed_batches),
        );

        (vec![Box::new(finder)], changed_batches)
    }

    #[test]
    fn discovery_entry_points_are_included_in_coverage() {
        let source = include_str!("find_project_dirs.rs");
        let find_marker = "#[cfg(not(tarpaulin_include))]\npub async fn find_project_dirs";

        assert!(!source.contains(find_marker));
    }

    #[tokio::test]
    async fn find_dispatches_negated_manifest_and_invokes_finalizer() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);
        fs::create_dir_all(temp_path.join("kept")).await.unwrap();
        fs::create_dir_all(temp_path.join("ignored")).await.unwrap();
        fs::write(
            temp_path.join("kept/package.json"),
            r#"{"name":"kept","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        fs::write(
            temp_path.join("ignored/package.json"),
            r#"{"name":"ignored","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        fs::write(temp_path.join("kept/index.js"), "export {};")
            .await
            .unwrap();
        git_add_and_commit(temp_path, "Initial commit");

        let visits = Arc::new(AtomicUsize::new(0));
        let finalizations = Arc::new(AtomicUsize::new(0));
        let changed_batches = Arc::new(Mutex::new(Vec::new()));
        let finder = RecordingNodeFinder::new(
            Arc::clone(&visits),
            Arc::clone(&finalizations),
            changed_batches,
        );
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(finder)];
        let config = Config {
            ignore: vec!["**/*".to_string(), "!kept/package.json".to_string()],
            ..Config::default()
        };

        find_project_dirs(&discover_repo(temp_path), &mut finders, &config, false)
            .await
            .unwrap();

        assert_eq!(visits.load(Ordering::SeqCst), 1);
        assert_eq!(finalizations.load(Ordering::SeqCst), 1);
        let projects = finders[0].projects();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name(), Some("kept"));
    }

    // A malformed `ignore` entry in `.changepacks/config.json` is the first
    // error a user hits after a typo, so the message that names the offending
    // pattern must survive refactors of `build_config_gitignore`. A reversed
    // character-class range (`[z-a]`) is rejected by
    // `GitignoreBuilder::add_line`, which drives the `with_context` arm.
    // (An unterminated `[` is NOT rejected — globset accepts it as a literal.)
    #[tokio::test]
    async fn find_project_dirs_reports_invalid_ignore_pattern_from_config() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);
        fs::write(
            temp_path.join("package.json"),
            r#"{"name":"root","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        git_add_and_commit(temp_path, "Initial commit");

        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];
        let config = Config {
            ignore: vec!["packages/[z-a]*.js".to_string()],
            ..Config::default()
        };

        let error = find_project_dirs(&discover_repo(temp_path), &mut finders, &config, false)
            .await
            .expect_err("malformed ignore pattern must abort discovery");
        let message = format!("{error:#}");

        assert!(
            message.contains("Invalid ignore pattern in config: packages/[z-a]*.js"),
            "unexpected error context: {message}"
        );

        temp_dir.close().unwrap();
    }

    // A freshly `git init`-ed repository has no `.git/index` until something is
    // staged, which is the only way to reach the `repo.index()` error arm in
    // `discover_project_dirs_with_gitignore`. Pin the actionable context so the
    // "add files to git" hint cannot silently disappear.
    #[tokio::test]
    async fn find_project_dirs_reports_missing_git_index() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);

        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        let error = find_project_dirs(
            &discover_repo(temp_path),
            &mut finders,
            &Config::default(),
            false,
        )
        .await
        .expect_err("a repository with no index must abort discovery");
        let message = format!("{error:#}");

        assert!(
            message.contains("Failed to get index, Please add files to git"),
            "unexpected error context: {message}"
        );

        temp_dir.close().unwrap();
    }

    // A bare repository is discovered successfully but exposes no work dir, so
    // it is the only way to reach the `work_dir()` -> None arm of
    // `find_project_dirs`. Pin the documented message so the branch cannot
    // silently lose its context.
    #[tokio::test]
    async fn find_project_dirs_rejects_bare_repository() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        run_git(temp_path, &["init", "--bare", "-b", "main"]);

        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        let error = find_project_dirs(
            &discover_repo(temp_path),
            &mut finders,
            &Config::default(),
            false,
        )
        .await
        .expect_err("a bare repository must abort discovery");
        let message = format!("{error:#}");

        assert!(
            message.contains("Not a working directory"),
            "unexpected error context: {message}"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn find_uses_directory_name_when_origin_is_absent() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);
        fs::write(temp_path.join("package.json"), r#"{"version":"1.0.0"}"#)
            .await
            .unwrap();
        git_add_and_commit(temp_path, "Initial commit");

        let expected = temp_path.file_name().unwrap().to_str().unwrap();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];
        find_project_dirs(
            &discover_repo(temp_path),
            &mut finders,
            &Config::default(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(finders[0].projects()[0].name(), Some(expected));
    }

    #[test]
    fn changed_path_collection_propagates_injected_status_error() {
        let status_paths = [Err(anyhow::anyhow!("injected status enumeration failure")
            .context("Failed to enumerate worktree status entry"))];

        let error = collect_dispatchable_paths(
            status_paths,
            None,
            "Failed to enumerate worktree status paths",
        )
        .expect_err("injected status error must reach the caller");

        assert!(
            format!("{error:#}").contains("Failed to enumerate worktree status paths"),
            "unexpected error context: {error:#}"
        );
        assert!(
            format!("{error:#}").contains("Failed to enumerate worktree status entry"),
            "missing status-entry context: {error:#}"
        );
    }

    #[test]
    fn changed_path_collection_propagates_injected_diff_path_error() {
        let diff_paths = [Err(anyhow::anyhow!("injected path conversion failure")
            .context("Failed to convert changed tree path"))];

        let error =
            collect_dispatchable_paths(diff_paths, None, "Failed to enumerate changed tree paths")
                .expect_err("injected diff path error must reach the caller");
        let message = format!("{error:#}");

        assert!(message.contains("Failed to enumerate changed tree paths"));
        assert!(message.contains("Failed to convert changed tree path"));
    }

    #[tokio::test]
    async fn find_deduplicates_base_diff_and_index_worktree_change() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);
        fs::create_dir_all(temp_path.join("packages/core"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/core/package.json"),
            r#"{"name":"core","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        let source = temp_path.join("packages/core/index.js");
        fs::write(&source, "export const value = 0;").await.unwrap();
        git_add_and_commit(temp_path, "Initial commit");
        run_git(temp_path, &["checkout", "-b", "feature"]);
        fs::write(&source, "export const value = 1;").await.unwrap();
        git_add_and_commit(temp_path, "Feature commit");
        fs::write(&source, "export const value = 2;").await.unwrap();
        run_git(temp_path, &["add", "packages/core/index.js"]);
        fs::write(&source, "export const value = 3;").await.unwrap();

        let visits = Arc::new(AtomicUsize::new(0));
        let finalizations = Arc::new(AtomicUsize::new(0));
        let changed_batches = Arc::new(Mutex::new(Vec::new()));
        let finder = RecordingNodeFinder::new(visits, finalizations, Arc::clone(&changed_batches));
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(finder)];

        find_project_dirs(
            &discover_repo(temp_path),
            &mut finders,
            &Config::default(),
            false,
        )
        .await
        .unwrap();

        let batches = changed_batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].iter().filter(|path| *path == &source).count(), 1);
        assert!(finders[0].projects()[0].is_changed());
    }

    #[tokio::test]
    async fn find_project_dirs_honors_ignore_for_tracked_base_branch_diff() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);
        fs::create_dir_all(temp_path.join("packages/core"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/core/package.json"),
            r#"{"name":"core","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        let ignored = temp_path.join("packages/core/ignored.js");
        fs::write(&ignored, "export const value = 0;")
            .await
            .unwrap();
        git_add_and_commit(temp_path, "Initial commit");
        run_git(temp_path, &["checkout", "-b", "feature"]);
        fs::write(&ignored, "export const value = 1;")
            .await
            .unwrap();
        git_add_and_commit(temp_path, "Ignored feature change");

        let (mut finders, changed_batches) = recording_node_finders();
        let config = Config {
            ignore: vec!["packages/core/ignored*.js".to_string()],
            ..Config::default()
        };

        find_project_dirs(&discover_repo(temp_path), &mut finders, &config, false)
            .await
            .unwrap();

        let batches = changed_batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert!(
            batches[0].is_empty(),
            "ignored base diff was dispatched: {:?}",
            batches[0]
        );
        assert!(!finders[0].projects()[0].is_changed());
    }

    #[tokio::test]
    async fn find_project_dirs_honors_ignore_for_staged_and_unstaged_status() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);
        fs::create_dir_all(temp_path.join("packages/core"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/core/package.json"),
            r#"{"name":"core","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        let staged = temp_path.join("packages/core/staged.js");
        let unstaged = temp_path.join("packages/core/unstaged.js");
        fs::write(&staged, "export const staged = 0;")
            .await
            .unwrap();
        fs::write(&unstaged, "export const unstaged = 0;")
            .await
            .unwrap();
        git_add_and_commit(temp_path, "Initial commit");
        fs::write(&staged, "export const staged = 1;")
            .await
            .unwrap();
        run_git(temp_path, &["add", "packages/core/staged.js"]);
        fs::write(&unstaged, "export const unstaged = 1;")
            .await
            .unwrap();

        let (mut finders, changed_batches) = recording_node_finders();
        let config = Config {
            ignore: vec!["packages/core/*.js".to_string()],
            ..Config::default()
        };

        find_project_dirs(&discover_repo(temp_path), &mut finders, &config, false)
            .await
            .unwrap();

        let batches = changed_batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert!(
            batches[0].is_empty(),
            "ignored staged or unstaged path was dispatched"
        );
        assert!(!finders[0].projects()[0].is_changed());
    }

    #[tokio::test]
    async fn find_project_dirs_honors_ignore_for_untracked_status() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);
        fs::create_dir_all(temp_path.join("packages/core"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/core/package.json"),
            r#"{"name":"core","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        git_add_and_commit(temp_path, "Initial commit");
        fs::write(
            temp_path.join("packages/core/ignored-untracked.js"),
            "export const value = 1;",
        )
        .await
        .unwrap();

        let (mut finders, changed_batches) = recording_node_finders();
        let config = Config {
            ignore: vec!["packages/core/ignored-*.js".to_string()],
            ..Config::default()
        };

        find_project_dirs(&discover_repo(temp_path), &mut finders, &config, false)
            .await
            .unwrap();

        let batches = changed_batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert!(
            batches[0].is_empty(),
            "ignored untracked path was dispatched"
        );
        assert!(!finders[0].projects()[0].is_changed());
    }

    #[tokio::test]
    async fn find_project_dirs_honors_ignore_negation_for_reincluded_child() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        init_git_repo(temp_path);
        fs::create_dir_all(temp_path.join("packages/core/generated"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/core/package.json"),
            r#"{"name":"core","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        let ignored = temp_path.join("packages/core/generated/ignored.js");
        let reincluded = temp_path.join("packages/core/generated/reincluded.js");
        fs::write(&ignored, "export const ignored = 0;")
            .await
            .unwrap();
        fs::write(&reincluded, "export const reincluded = 0;")
            .await
            .unwrap();
        git_add_and_commit(temp_path, "Initial commit");
        fs::write(&ignored, "export const ignored = 1;")
            .await
            .unwrap();
        fs::write(&reincluded, "export const reincluded = 1;")
            .await
            .unwrap();

        let (mut finders, changed_batches) = recording_node_finders();
        let config = Config {
            ignore: vec![
                "packages/core/generated/**".to_string(),
                "!packages/core/generated/reincluded.js".to_string(),
            ],
            ..Config::default()
        };

        find_project_dirs(&discover_repo(temp_path), &mut finders, &config, false)
            .await
            .unwrap();

        let batches = changed_batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], vec![reincluded]);
        assert!(finders[0].projects()[0].is_changed());
    }

    #[test]
    fn test_finder_can_visit_path_matches_manifest_name() {
        let finder = NodeProjectFinder::new();

        assert!(project_files_can_visit_path(
            finder.project_files(),
            Path::new("apps/a/package.json"),
            "package.json"
        ));
        assert!(!project_files_can_visit_path(
            finder.project_files(),
            Path::new("apps/a/index.ts"),
            "index.ts"
        ));
    }

    #[test]
    fn test_finder_can_visit_path_matches_csharp_extension_case_insensitive() {
        let project_files = [".csproj"];

        assert!(project_files_can_visit_path(
            &project_files,
            Path::new("src/App.csproj"),
            "App.csproj"
        ));
        assert!(project_files_can_visit_path(
            &project_files,
            Path::new("src/App.CSPROJ"),
            "App.CSPROJ"
        ));
        assert!(project_files_can_visit_path(
            &project_files,
            Path::new("src/App.CsProj"),
            "App.CsProj"
        ));
        assert!(!project_files_can_visit_path(
            &project_files,
            Path::new("src/App.sln"),
            "App.sln"
        ));
    }

    // The extension form is scoped to the extension: it must NOT sweep up an
    // unrelated manifest that another finder owns. If it did, the C# finder
    // would be dispatched every `package.json` in the tree.
    #[test]
    fn test_finder_can_visit_path_extension_entry_ignores_other_manifest_names() {
        let project_files = [".csproj"];

        for (path, file_name) in [
            ("apps/a/package.json", "package.json"),
            ("crates/a/Cargo.toml", "Cargo.toml"),
            ("src/csproj", "csproj"),
        ] {
            assert!(
                !project_files_can_visit_path(&project_files, Path::new(path), file_name),
                "extension entry \".csproj\" must not claim {path}"
            );
        }
    }

    // The bare-file-name form is an exact, case-sensitive whole-name compare —
    // never a suffix or extension match. `settings.package.json` shares the
    // extension and ends with the manifest name, yet is not a manifest.
    #[test]
    fn test_finder_can_visit_path_plain_name_entry_matches_only_exact_name() {
        let project_files = ["package.json"];

        assert!(project_files_can_visit_path(
            &project_files,
            Path::new("apps/a/package.json"),
            "package.json"
        ));

        for (path, file_name) in [
            ("apps/a/settings.package.json", "settings.package.json"),
            ("apps/a/package.json.bak", "package.json.bak"),
            ("apps/a/tsconfig.json", "tsconfig.json"),
            ("apps/a/Package.json", "Package.json"),
        ] {
            assert!(
                !project_files_can_visit_path(&project_files, Path::new(path), file_name),
                "file-name entry \"package.json\" must not claim {path}"
            );
        }
    }

    #[tokio::test]
    async fn test_find_project_dirs_basic() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Create a package.json file
        fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name(), Some("test"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_with_ignore() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Create packages
        fs::create_dir_all(temp_path.join("packages/core"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/core/package.json"),
            r#"{"name": "core", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        fs::create_dir_all(temp_path.join("packages/ignored"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/ignored/package.json"),
            r#"{"name": "ignored", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config {
            ignore: vec!["packages/ignored/**".to_string()],
            ..Default::default()
        };
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name(), Some("core"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_with_changed_files() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Create a package.json file
        fs::create_dir_all(temp_path.join("packages/core"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/core/package.json"),
            r#"{"name": "core", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();
        fs::write(
            temp_path.join("packages/core/index.js"),
            "console.log('hello');",
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        // Modify a file (unstaged change)
        fs::write(
            temp_path.join("packages/core/index.js"),
            "console.log('modified');",
        )
        .await
        .unwrap();

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);
        // The project should be marked as changed
        assert!(projects[0].is_changed());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_empty_ignore() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        // Empty ignore list
        let config = Config {
            ignore: vec![],
            ..Default::default()
        };
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_multiple_packages() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Create multiple packages
        for name in ["core", "utils", "cli"] {
            fs::create_dir_all(temp_path.join(format!("packages/{}", name)))
                .await
                .unwrap();
            fs::write(
                temp_path.join(format!("packages/{}/package.json", name)),
                format!(r#"{{"name": "{}", "version": "1.0.0"}}"#, name),
            )
            .await
            .unwrap();
        }

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 3);

        let names: Vec<_> = projects.iter().filter_map(|p| p.name()).collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"utils"));
        assert!(names.contains(&"cli"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_diff_from_main() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Create initial package
        fs::create_dir_all(temp_path.join("packages/core"))
            .await
            .unwrap();
        fs::write(
            temp_path.join("packages/core/package.json"),
            r#"{"name": "core", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();
        fs::write(
            temp_path.join("packages/core/index.js"),
            "console.log('initial');",
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        // Create a feature branch and make changes
        run_git(temp_path, &["checkout", "-b", "feature"]);

        fs::write(
            temp_path.join("packages/core/index.js"),
            "console.log('feature change');",
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Feature commit");

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);
        // The project should be marked as changed (diff from main)
        assert!(projects[0].is_changed());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_local_divergent_history_uses_merge_base() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);
        for name in ["a", "b", "uncommitted"] {
            let package_dir = temp_path.join(format!("packages/{name}"));
            fs::create_dir_all(&package_dir).await.unwrap();
            fs::write(
                package_dir.join("package.json"),
                format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
            )
            .await
            .unwrap();
            fs::write(package_dir.join("index.js"), "export const value = 0;")
                .await
                .unwrap();
        }
        git_add_and_commit(temp_path, "Common ancestor");

        run_git(temp_path, &["checkout", "-b", "feature"]);
        fs::write(
            temp_path.join("packages/a/index.js"),
            "export const value = 'feature';",
        )
        .await
        .unwrap();
        git_add_and_commit(temp_path, "Feature-only package A change");

        run_git(temp_path, &["checkout", "main"]);
        fs::write(
            temp_path.join("packages/b/index.js"),
            "export const value = 'upstream';",
        )
        .await
        .unwrap();
        git_add_and_commit(temp_path, "Upstream-only package B change");

        run_git(temp_path, &["checkout", "feature"]);
        fs::write(
            temp_path.join("packages/uncommitted/index.js"),
            "export const value = 'working tree';",
        )
        .await
        .unwrap();

        let repo = discover_repo(temp_path);
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &Config::default(), false)
            .await
            .unwrap();

        let projects: Vec<_> = finders
            .iter()
            .flat_map(|finder| finder.projects())
            .collect();
        let is_changed = |name: &str| {
            projects
                .iter()
                .find(|project| project.name() == Some(name))
                .unwrap_or_else(|| panic!("project {name} was not discovered"))
                .is_changed()
        };
        assert!(is_changed("a"), "feature-only package A was excluded");
        assert!(
            !is_changed("b"),
            "upstream-only package B was included in the feature diff"
        );
        assert!(
            is_changed("uncommitted"),
            "uncommitted worktree change was excluded"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_remote_branch() {
        // Create a "remote" repository
        let remote_dir = TempDir::new().unwrap();
        let remote_path = remote_dir.path();

        init_git_repo(remote_path);

        fs::create_dir_all(remote_path.join("packages/core"))
            .await
            .unwrap();
        fs::write(
            remote_path.join("packages/core/package.json"),
            r#"{"name": "core", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();
        fs::write(
            remote_path.join("packages/core/index.js"),
            "console.log('initial');",
        )
        .await
        .unwrap();

        git_add_and_commit(remote_path, "Initial commit");

        // Create a local clone
        let local_dir = TempDir::new().unwrap();
        let local_path = local_dir.path();

        run_git(local_path, &["clone", remote_path.to_str().unwrap(), "."]);

        // Configure git user for the local clone
        run_git(local_path, &["config", "user.email", "test@test.com"]);
        run_git(local_path, &["config", "user.name", "Test"]);

        // Create a feature branch with changes
        run_git(local_path, &["checkout", "-b", "feature"]);

        fs::write(
            local_path.join("packages/core/index.js"),
            "console.log('feature change');",
        )
        .await
        .unwrap();

        git_add_and_commit(local_path, "Feature commit");

        let repo = discover_repo(local_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        // Test with remote=true to hit lines 88-90
        find_project_dirs(&repo, &mut finders, &config, true)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].is_changed());

        local_dir.close().unwrap();
        remote_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_remote_divergent_history_uses_merge_base() {
        let remote_dir = TempDir::new().unwrap();
        let remote_path = remote_dir.path();

        init_git_repo(remote_path);
        for name in ["a", "b", "uncommitted"] {
            let package_dir = remote_path.join(format!("packages/{name}"));
            fs::create_dir_all(&package_dir).await.unwrap();
            fs::write(
                package_dir.join("package.json"),
                format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
            )
            .await
            .unwrap();
            fs::write(package_dir.join("index.js"), "export const value = 0;")
                .await
                .unwrap();
        }
        git_add_and_commit(remote_path, "Common ancestor");

        let local_dir = TempDir::new().unwrap();
        let local_path = local_dir.path();
        run_git(local_path, &["clone", remote_path.to_str().unwrap(), "."]);
        run_git(local_path, &["config", "user.email", "test@test.com"]);
        run_git(local_path, &["config", "user.name", "Test"]);
        run_git(local_path, &["checkout", "-b", "feature"]);
        fs::write(
            local_path.join("packages/a/index.js"),
            "export const value = 'feature';",
        )
        .await
        .unwrap();
        git_add_and_commit(local_path, "Feature-only package A change");

        fs::write(
            remote_path.join("packages/b/index.js"),
            "export const value = 'upstream';",
        )
        .await
        .unwrap();
        git_add_and_commit(remote_path, "Upstream-only package B change");
        run_git(local_path, &["fetch", "origin"]);

        fs::write(
            local_path.join("packages/uncommitted/index.js"),
            "export const value = 'working tree';",
        )
        .await
        .unwrap();

        let repo = discover_repo(local_path);
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &Config::default(), true)
            .await
            .unwrap();

        let projects: Vec<_> = finders
            .iter()
            .flat_map(|finder| finder.projects())
            .collect();
        let is_changed = |name: &str| {
            projects
                .iter()
                .find(|project| project.name() == Some(name))
                .unwrap_or_else(|| panic!("project {name} was not discovered"))
                .is_changed()
        };
        assert!(is_changed("a"), "feature-only package A was excluded");
        assert!(
            !is_changed("b"),
            "upstream-only package B was included in the feature diff"
        );
        assert!(
            is_changed("uncommitted"),
            "uncommitted worktree change was excluded"
        );

        local_dir.close().unwrap();
        remote_dir.close().unwrap();
    }

    // Symmetry gate for the local vs. remote base-branch error message.
    // The local (`refs/heads/<base>`) arm already wraps its `find_reference`
    // with `.with_context(|| ...)` so users get "base branch 'main' not
    // found in local refs". Historically the remote arm surfaced the raw
    // gix error and left users guessing whether they simply forgot to
    // `git fetch`. This test locks in the mirrored context on the remote
    // arm: a repo that has an `origin` remote configured but no
    // `refs/remotes/origin/<base>` ref (never fetched) MUST surface an
    // anyhow error chain whose text contains both "remote base branch" and
    // the base branch name so `--remote` failures are self-diagnosing.
    #[tokio::test]
    async fn test_find_project_dirs_remote_branch_missing_ref_has_context() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Add an origin remote URL but never fetch — so refs/remotes/origin/main
        // stays absent and the `find_reference` call is the failure surface.
        run_git(
            temp_path,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/no-such-repo.git",
            ],
        );

        fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config::default(); // base_branch defaults to "main"
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        let result = find_project_dirs(&repo, &mut finders, &config, true).await;
        let err = result.expect_err("expected remote base branch lookup to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("remote base branch"),
            "expected error to mention 'remote base branch', got: {msg}"
        );
        assert!(
            msg.contains(&config.base_branch),
            "expected error to mention the base branch name '{}', got: {msg}",
            config.base_branch
        );

        temp_dir.close().unwrap();
    }

    // Sibling gate to the missing-ref test above. When `--remote` is used in a
    // repo that has NO `origin` remote configured at all, the `find_remote`
    // call itself is the failure surface (not `find_reference`). Historically
    // it surfaced the raw gix "remote not found" error, leaving users unsure
    // why `--remote` failed. This test locks in the anyhow context so the error
    // chain names the missing "origin" remote.
    //
    // The assertion pins the FULL context sentence, not just the word
    // "origin": the underlying gix error already contains "origin" on its
    // own, so a substring check for "origin" alone passes even when the
    // `.context(..)` at the `find_remote("origin")` call site is deleted
    // outright. Only the exact-sentence assertion actually exercises that
    // context string and its `--remote` remediation hint.
    #[tokio::test]
    async fn test_find_project_dirs_remote_missing_origin_has_context() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Deliberately do NOT add an `origin` remote, so `find_remote("origin")`
        // is the failure surface when `remote=true`.
        fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        let result = find_project_dirs(&repo, &mut finders, &config, true).await;
        let err = result.expect_err("expected missing origin remote lookup to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(
                "Git remote 'origin' is not configured; --remote requires an 'origin' remote"
            ),
            "expected the missing-origin context sentence, got: {msg}"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_project_dirs_sets_name_from_remote_origin() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Add origin remote with a URL containing the repo name
        run_git(
            temp_path,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/testuser/my-cool-repo.git",
            ],
        );

        // Create a package.json WITHOUT a name field
        fs::write(temp_path.join("package.json"), r#"{"version": "1.0.0"}"#)
            .await
            .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);
        // Name should be extracted from the remote origin URL
        assert_eq!(projects[0].name(), Some("my-cool-repo"));
    }

    #[tokio::test]
    async fn test_find_project_dirs_sets_name_from_ssh_remote() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Add origin remote with SSH URL
        run_git(
            temp_path,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:testuser/ssh-repo.git",
            ],
        );

        // Create a package.json WITHOUT a name field
        fs::write(temp_path.join("package.json"), r#"{"version": "1.0.0"}"#)
            .await
            .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);
        // Name should be extracted from the SSH remote URL
        assert_eq!(projects[0].name(), Some("ssh-repo"));
    }

    #[tokio::test]
    async fn test_find_project_dirs_name_not_overwritten_by_remote() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Add origin remote
        run_git(
            temp_path,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/testuser/remote-name.git",
            ],
        );

        // Create a package.json WITH a name field
        fs::write(
            temp_path.join("package.json"),
            r#"{"name": "explicit-name", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        let projects: Vec<_> = finders.iter().flat_map(|f| f.projects()).collect();
        assert_eq!(projects.len(), 1);
        // Explicit name should NOT be overwritten by remote repo name
        assert_eq!(projects[0].name(), Some("explicit-name"));
    }

    // Every other no-name fallback test commits exactly ONE name-less manifest,
    // so `targets.split_last_mut()` yields an empty `rest` and only the
    // move-into-`last` arm ever runs. This case commits TWO name-less
    // manifests, which is the only way to execute the `for project in rest`
    // clone loop: with the loop removed, `a/package.json` keeps `name() ==
    // None` while `b/package.json` still gets the remote-derived name.
    #[tokio::test]
    async fn test_find_project_dirs_sets_remote_name_on_every_no_name_project() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);
        run_git(
            temp_path,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/testuser/shared-repo-name.git",
            ],
        );

        // Two manifests in different directories, BOTH without a name field.
        for dir in ["a", "b"] {
            fs::create_dir_all(temp_path.join(dir)).await.unwrap();
            fs::write(
                temp_path.join(dir).join("package.json"),
                r#"{"version": "1.0.0"}"#,
            )
            .await
            .unwrap();
        }

        git_add_and_commit(temp_path, "Initial commit");

        let repo = discover_repo(temp_path);
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        find_project_dirs(&repo, &mut finders, &config, false)
            .await
            .unwrap();

        // `projects()` is HashMap-ordered, so key by relative path instead of
        // index to keep the assertion deterministic.
        let mut named: Vec<(String, Option<String>)> = finders
            .iter()
            .flat_map(|f| f.projects())
            .map(|project| {
                (
                    project.relative_path().to_string_lossy().replace('\\', "/"),
                    project.name().map(String::from),
                )
            })
            .collect();
        named.sort();

        assert_eq!(
            named,
            vec![
                (
                    "a/package.json".to_string(),
                    Some("shared-repo-name".to_string())
                ),
                (
                    "b/package.json".to_string(),
                    Some("shared-repo-name".to_string())
                ),
            ],
            "every no-name project must receive the repo-derived name"
        );
    }
}
