use crate::get_relative_path;
use anyhow::{Context, Result};
use changepacks_core::ProjectFinder;
use gix::{ThreadSafeRepository, bstr::ByteSlice, features::progress};
use std::path::Path;

/// Find project directories containing specific files from git tracked files
pub async fn find_project_dirs(
    repo: &ThreadSafeRepository,
    project_finders: &mut [Box<dyn ProjectFinder>],
) -> Result<()> {
    // Get git root for relative path conversion
    let git_root_path = repo.work_dir().context("Not a working directory")?;

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
        .filter_map(|entry| {
            entry.ok().and_then(|entry| {
                entry
                    .rela_path()
                    .to_path()
                    .ok()
                    .map(|path| path.to_path_buf())
            })
        })
        .collect::<Vec<_>>();
    // diff from main branch
    let main_tree = repo
        .find_reference("refs/heads/main")?
        .id()
        .object()?
        .try_into_commit()?
        .tree_id()?
        .object()?
        .try_into_tree()?;
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
                .map(|path| path.to_path_buf())
        })
        .collect::<Vec<_>>();

    for file in changed_files.iter().chain(diff.iter()) {
        for finder in project_finders.iter_mut() {
            finder.check_changed(&git_root_path.join(file))?;
        }
    }

    Ok(())
}
