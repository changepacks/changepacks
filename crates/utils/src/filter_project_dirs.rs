use anyhow::Result;
use core::proejct_finder::ProjectFinder;
use gix::Repository;
use std::path::Path;

/// Find project directories containing specific files from git tracked files
pub fn find_project_dirs(
    repo: &Repository,
    project_finders: &mut [Box<dyn ProjectFinder>],
) -> Result<()> {
    let index = repo.index()?;

    // Get git root for relative path conversion
    let git_root = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("Not a working directory"))?;
    let git_root_path = Path::new(git_root);

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

        for project_finder in project_finders.iter_mut() {
            project_finder.visit(&abs_path);
        }
    }

    Ok(())
}
