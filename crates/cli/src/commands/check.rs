use changepack_core::project::Project;

use anyhow::{Context, Result};
use clap::Args;
use utils::{display_project, find_current_git_repo, find_project_dirs};

use crate::{finders::get_finders, options::FilterOptions};

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct CheckArgs {
    #[arg(short, long)]
    filter: Option<FilterOptions>,
}

/// Check project status
pub async fn handle_check(args: &CheckArgs) -> Result<()> {
    let repo = find_current_git_repo()?;
    // check if changepack.json exists
    let changepack_file = repo
        .workdir()
        .context("Failed to find current git repository")?
        .join(".changepack/changepack.json");
    if !changepack_file.exists() {
        Err(anyhow::anyhow!("Changepack project not initialized"))
    } else {
        println!("Changepack project found in {:?}", changepack_file);

        let mut project_finders = get_finders();

        find_project_dirs(&repo, &mut project_finders).await?;

        let mut projects = project_finders
            .iter()
            .flat_map(|finder| finder.projects())
            .collect::<Vec<_>>();
        if let Some(filter) = &args.filter {
            projects.retain(|project| match filter {
                FilterOptions::Workspace => matches!(project, Project::Workspace(_)),
                FilterOptions::Package => matches!(project, Project::Package(_)),
            });
        }
        println!("Found {} projects", projects.len());
        for project in projects {
            println!("{}", display_project(project));
        }
        Ok(())
    }
}
