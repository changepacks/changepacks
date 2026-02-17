use crate::get_relative_path;
use anyhow::{Context, Result};
use changepacks_core::{Config, ProjectFinder};
use gix::{ThreadSafeRepository, bstr::ByteSlice, features::progress};
use ignore::gitignore::GitignoreBuilder;
use std::path::Path;

/// Find project directories containing specific files from git tracked files
///
/// # Errors
/// Returns error if git operations fail, gitignore parsing fails, or project visiting fails.
pub async fn find_project_dirs(
    repo: &ThreadSafeRepository,
    project_finders: &mut [Box<dyn ProjectFinder>],
    config: &Config,
    remote: bool,
) -> Result<()> {
    // Get git root for relative path conversion
    let git_root_path = repo.work_dir().context("Not a working directory")?;

    // Build gitignore from config patterns (supports ! negation patterns)
    let gitignore = if config.ignore.is_empty() {
        None
    } else {
        let mut builder = GitignoreBuilder::new(git_root_path);
        for pattern in &config.ignore {
            builder.add_line(None, pattern)?;
        }
        Some(builder.build()?)
    };

    let repo = repo.to_thread_local();
    let index = repo
        .index()
        .context("Failed to get index, Please add files to git")?;
    // Iterate through git tracked files and find matching project files
    for entry in index.entries() {
        let file_path = entry.path(&index);
        let file_path_str = file_path.to_string();
        let path = Path::new(&file_path_str);

        // Check if this file matches any of the project files
        // Insert absolute path using git_root_path.join(parent)
        let abs_path = git_root_path.join(path);
        let rel_path = get_relative_path(git_root_path, &abs_path)?;

        // Skip if path matches ignore patterns (gitignore supports ! negation)
        if let Some(ref gitignore) = gitignore
            && gitignore.matched(&rel_path, false).is_ignore()
        {
            continue;
        }

        futures::future::join_all(
            project_finders
                .iter_mut()
                .map(async |finder| finder.visit(&abs_path, &rel_path).await),
        )
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    }

    // Post-visit finalization (resolves deferred state like workspace-inherited versions)
    for finder in project_finders.iter_mut() {
        finder.finalize().await?;
    }

    // Fallback: set git repo name for projects with no name
    // Priority: remote origin repo name > directory name
    let repo_name = repo
        .try_find_remote("origin")
        .and_then(|r| r.ok())
        .and_then(|remote| {
            let url = remote.url(gix::remote::Direction::Fetch)?;
            let path = url.path.to_string();
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
    if let Some(ref repo_name) = repo_name {
        for finder in project_finders.iter_mut() {
            for project in finder.projects_mut() {
                if project.name().is_none() {
                    project.set_name(repo_name.clone());
                }
            }
        }
    }

    let changed_files = repo
        .status(progress::Discard)?
        .into_index_worktree_iter(Vec::new())?
        .filter_map(|entry| {
            entry.ok().and_then(|entry| {
                entry
                    .rela_path()
                    .to_path()
                    .ok()
                    .map(std::path::Path::to_path_buf)
            })
        })
        .collect::<Vec<_>>();
    // diff from main branch
    let main_tree = if remote {
        repo.find_remote("origin")?
            .repo
            .find_reference(&format!("refs/remotes/origin/{}", config.base_branch))?
            .id()
            .object()?
            .try_into_commit()?
            .tree_id()?
            .object()?
            .try_into_tree()?
    } else {
        repo.find_reference(&format!("refs/heads/{}", config.base_branch))
            .with_context(|| {
                format!(
                    "base branch '{}' not found in local refs",
                    config.base_branch
                )
            })?
            .id()
            .object()?
            .try_into_commit()?
            .tree_id()?
            .object()?
            .try_into_tree()?
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
        .collect::<Vec<_>>();

    for file in changed_files.iter().chain(diff.iter()) {
        for finder in project_finders.iter_mut() {
            finder.check_changed(&git_root_path.join(file))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_node::finder::NodeProjectFinder;
    use tempfile::TempDir;
    use tokio::fs;

    fn init_git_repo(path: &Path) {
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .unwrap();
    }

    fn git_add_and_commit(path: &Path, message: &str) {
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(path)
            .output()
            .unwrap();
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
        std::process::Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(temp_path)
            .output()
            .unwrap();

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

        std::process::Command::new("git")
            .args(["clone", remote_path.to_str().unwrap(), "."])
            .current_dir(local_path)
            .output()
            .unwrap();

        // Configure git user for the local clone
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(local_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(local_path)
            .output()
            .unwrap();

        // Create a feature branch with changes
        std::process::Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(local_path)
            .output()
            .unwrap();

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

    #[tokio::test]
    async fn test_find_project_dirs_sets_name_from_remote_origin() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        init_git_repo(temp_path);

        // Add origin remote with a URL containing the repo name
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/testuser/my-cool-repo.git",
            ])
            .current_dir(temp_path)
            .output()
            .unwrap();

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
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:testuser/ssh-repo.git",
            ])
            .current_dir(temp_path)
            .output()
            .unwrap();

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
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/testuser/remote-name.git",
            ])
            .current_dir(temp_path)
            .output()
            .unwrap();

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
