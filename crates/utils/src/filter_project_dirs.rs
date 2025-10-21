use crate::get_relative_path;
use anyhow::{Context, Result};
use changepacks_core::proejct_finder::ProjectFinder;
use gix::{ThreadSafeRepository, bstr::ByteSlice, features::progress};
use std::path::Path;

/// Find project directories containing specific files from git tracked files
pub async fn find_project_dirs(
    repo: &ThreadSafeRepository,
    project_finders: &mut [Box<dyn ProjectFinder>],
) -> Result<()> {
    // Get git root for relative path conversion
    let git_root_path = repo
        .work_dir()
        .ok_or_else(|| anyhow::anyhow!("Not a working directory"))?;

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
        let abs_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            git_root_path.join(path)
        };

        futures::future::join_all(project_finders.iter_mut().map(async |finder| {
            finder
                .visit(&abs_path, &get_relative_path(git_root_path, &abs_path)?)
                .await
        }))
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    }

    let changed_files = repo
        .status(progress::Discard)?
        .into_index_worktree_iter(Vec::new())?
        .collect::<Result<Vec<_>, _>>()?;
    for file in changed_files {
        let path = file
            .rela_path()
            .to_path()
            .context("Failed to convert path to std path")?;
        for finder in project_finders.iter_mut() {
            finder.check_changed(&git_root_path.join(path))?;
        }
    }

    Ok(())
}
