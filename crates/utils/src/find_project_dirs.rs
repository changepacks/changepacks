use anyhow::{Context, Result};
use changepacks_core::{
    Config, Project, ProjectFinder, contains_changepacks_component, has_extension_ignore_ascii_case,
};
use gix::{ThreadSafeRepository, bstr::ByteSlice, features::progress};
use ignore::gitignore::GitignoreBuilder;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// Resolve a git ref (local or remote) to its committed tree by peeling
/// `ref → id → commit → tree_id → tree`. Extracted so the local-branch and
/// remote-branch arms of `find_project_dirs` no longer duplicate the six-step
/// gix chain — a future gix API tweak (e.g. `peel_to_tree()` becoming an
/// upstream helper) then only needs to be applied here. Same lifetime story on
/// both sides: the returned `gix::Tree` borrows the same repository as the
/// input `gix::Reference`.
fn peel_to_tree(reference: gix::Reference<'_>) -> Result<gix::Tree<'_>> {
    Ok(reference
        .id()
        .object()?
        .try_into_commit()?
        .tree_id()?
        .object()?
        .try_into_tree()?)
}

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

/// Discover project directories containing specific files from git tracked
/// files — the discovery-only walk, with NO git change detection.
///
/// This is the first half of [`find_project_dirs`]: build the gitignore
/// matcher from `config.ignore`, walk the git index dispatching each tracked
/// file to every finder that can visit it, run post-visit finalization, and
/// apply the remote-origin / directory-name fallback for projects with no
/// name. It deliberately skips the base-branch diff / worktree-status pass, so
/// `is_changed` is never populated — use it for callers that only read the
/// discovered paths/names/deps/versions and never inspect `is_changed`.
///
/// # Errors
/// Returns error if git operations fail, gitignore parsing fails, or project visiting fails.
///
/// Excluded from coverage: orchestrates real `gix` operations (index walk,
/// finalize, remote-origin name lookup); the inner helpers
/// (`get_relative_path`, `gitignore matching`, finder visit) are covered by
/// their own unit tests. End-to-end exercise happens via the cli integration
/// tests.
#[cfg(not(tarpaulin_include))]
pub async fn discover_project_dirs(
    repo: &ThreadSafeRepository,
    project_finders: &mut [Box<dyn ProjectFinder>],
    config: &Config,
) -> Result<()> {
    // Get git root for relative path conversion
    let git_root_path = repo.work_dir().context("Not a working directory")?;

    // Build gitignore from config patterns (supports ! negation patterns)
    let gitignore = if config.ignore.is_empty() {
        None
    } else {
        let mut builder = GitignoreBuilder::new(git_root_path);
        for pattern in &config.ignore {
            builder
                .add_line(None, pattern)
                .with_context(|| format!("Invalid ignore pattern in config: {pattern}"))?;
        }
        Some(
            builder
                .build()
                .context("Failed to build ignore matcher from config patterns")?,
        )
    };

    let repo = repo.to_thread_local();
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
        if let Some(ref gitignore) = gitignore
            && gitignore.matched(path, false).is_ignore()
        {
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
    let mut targets: Vec<&mut Project> = project_finders
        .iter_mut()
        .flat_map(|f| f.projects_mut())
        .filter(|p| p.name().is_none())
        .collect();
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

    Ok(())
}

/// Find project directories containing specific files from git tracked files
///
/// # Errors
/// Returns error if git operations fail, gitignore parsing fails, or project visiting fails.
///
/// Excluded from coverage: orchestrates real `gix` operations (index walk,
/// status, diff against base branch, ref resolution); the inner helpers
/// (`get_relative_path`, `gitignore matching`, finder visit/check_changed)
/// are covered by their own unit tests. End-to-end exercise happens via
/// the cli integration tests.
#[cfg(not(tarpaulin_include))]
pub async fn find_project_dirs(
    repo: &ThreadSafeRepository,
    project_finders: &mut [Box<dyn ProjectFinder>],
    config: &Config,
    remote: bool,
) -> Result<()> {
    discover_project_dirs(repo, project_finders, config).await?;

    // The change-detection tail re-establishes the git root and a thread-local
    // repo handle that `discover_project_dirs` scoped to its own discovery
    // walk, then runs the base-branch diff + worktree-status pass that actually
    // populates `is_changed`.
    let git_root_path = repo.work_dir().context("Not a working directory")?;
    let repo = repo.to_thread_local();

    // diff from main branch — compute FIRST so `diff.len()` can seed the
    // `unique_files` capacity below without an intermediate
    // `changed_files: Vec<PathBuf>` allocation for the status entries.
    let main_tree = if remote {
        peel_to_tree(
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
        peel_to_tree(
            repo.find_reference(&format!("refs/heads/{}", config.base_branch))
                .with_context(|| {
                    format!(
                        "base branch '{}' not found in local refs",
                        config.base_branch
                    )
                })?,
        )?
    };
    let head_tree = repo.head_tree()?;
    let diff = repo
        .diff_tree_to_tree(
            Some(&head_tree),
            Some(&main_tree),
            gix::diff::Options::default(),
        )?
        .into_iter()
        .filter_map(|change| {
            change
                .location()
                .to_path()
                .ok()
                .map(std::path::Path::to_path_buf)
        })
        .filter(|path| !contains_changepacks_component(path))
        .collect::<Vec<_>>();

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
    //
    // Preallocate: `HashSet::from_iter` (via `collect`) does NOT use
    // `size_hint` to reserve capacity (unlike `Vec`), so it incurs
    // geometric-doubling reallocations. The intermediate
    // `changed_files: Vec<PathBuf>` from the status iterator was pure
    // waste — its sole consumer was `unique_files.extend(changed_files)`,
    // so we skip the Vec entirely and extend the HashSet directly from
    // the status iterator. `diff.len() * 2` is a conservative estimate
    // (typical live-edit repos have status entries in the same order of
    // magnitude as base-branch diff entries) that avoids reserving too
    // little without unbounded over-allocation.
    let mut unique_files: HashSet<PathBuf> = HashSet::with_capacity(diff.len() * 2);
    unique_files.extend(
        repo.status(progress::Discard)?
            .into_index_worktree_iter(Vec::new())?
            .filter_map(|entry| {
                entry.ok().and_then(|entry| {
                    entry
                        .rela_path()
                        .to_path()
                        .ok()
                        .map(std::path::Path::to_path_buf)
                        .filter(|path| !contains_changepacks_component(path))
                })
            }),
    );
    unique_files.extend(diff);

    // Resolve every unique changed file to an absolute path ONCE, then dispatch
    // the whole batch to each finder. The previous file-major nested loop
    // rebuilt a fresh `Vec<&mut Project>` (via `projects_mut()`) for every
    // (file, finder) pair — `F` files × `M` finders allocations. `check_changed_many`
    // takes one `projects_mut()` snapshot per finder, dropping that to `M`
    // Vec allocations total. Order-flip safety (project-major vs file-major) is
    // guaranteed by `Project::check_changed` monotonicity — see its doc comment
    // on `ProjectFinder::check_changed_many`.
    let abs_paths: Vec<PathBuf> = unique_files
        .iter()
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
    use crate::test_support::{git_add_and_commit, init_git_repo, run_git};
    use changepacks_node::finder::NodeProjectFinder;
    use tempfile::TempDir;
    use tokio::fs;

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
        assert!(!project_files_can_visit_path(
            &project_files,
            Path::new("src/App.sln"),
            "App.sln"
        ));
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(local_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
        let config = Config::default();
        let mut finders: Vec<Box<dyn ProjectFinder>> = vec![Box::new(NodeProjectFinder::new())];

        let result = find_project_dirs(&repo, &mut finders, &config, true).await;
        let err = result.expect_err("expected missing origin remote lookup to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("origin"),
            "expected error to mention 'origin', got: {msg}"
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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

        let repo = gix::discover(temp_path).unwrap().into_sync();
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
}
