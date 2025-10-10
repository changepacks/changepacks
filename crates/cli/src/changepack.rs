use core::proejct_finder::ProjectFinder;

use anyhow::Result;
use node::NodeProjectFinder;
use python::PythonProjectFinder;
use rust::RustProjectFinder;
use utils::{filter_project_dirs::find_project_dirs, find_current_git_repo::find_current_git_repo};

pub fn handle_changepack() -> Result<()> {
    let mut project_finders: [Box<dyn ProjectFinder>; 3] = [
        Box::new(NodeProjectFinder::new()),
        Box::new(RustProjectFinder::new()),
        Box::new(PythonProjectFinder::new()),
    ];

    // collect all projects
    let repo = find_current_git_repo()?;
    find_project_dirs(&repo, &mut project_finders)?;

    println!(
        "Collecting projects from: {:?}",
        project_finders
            .iter()
            .map(|finder| finder.projects())
            .collect::<Vec<_>>()
    );
    println!("Collecting projects from current directory");
    Ok(())
}
