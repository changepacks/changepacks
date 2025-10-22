use changepacks_core::Project;

use anyhow::{Context, Result};
use clap::Args;
use utils::{
    display_update, find_current_git_repo, find_project_dirs, gen_changepack_result_map,
    gen_update_map, get_changepacks_dir, get_relative_path,
};

use crate::{
    finders::get_finders,
    options::{FilterOptions, FormatOptions},
};

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct CheckArgs {
    #[arg(short, long)]
    filter: Option<FilterOptions>,

    #[arg(long, default_value = "stdout")]
    format: FormatOptions,
}

/// Check project status
pub async fn handle_check(args: &CheckArgs) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let repo = find_current_git_repo(&current_dir)?;
    let repo_root_path = repo.work_dir().context("Not a working directory")?;
    // check if changepacks.json exists
    let changepacks_file = get_changepacks_dir(&current_dir)?.join("changepacks.json");
    if !changepacks_file.exists() {
        Err(anyhow::anyhow!("changepacks project not initialized"))
    } else {
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
        projects.sort();
        if let FormatOptions::Stdout = args.format {
            println!("Found {} projects", projects.len());
        }
        let update_map = gen_update_map(&current_dir).await?;
        match args.format {
            FormatOptions::Stdout => {
                for project in projects {
                    println!(
                        "{}",
                        format!("{}", project).replace(
                            project.version().unwrap_or("unknown"),
                            &if let Some(update_type) =
                                update_map.get(&get_relative_path(repo_root_path, project.path())?)
                            {
                                display_update(project.version(), update_type.0.clone())?
                            } else {
                                project.version().unwrap_or("unknown").to_string()
                            }
                        ),
                    )
                }
            }
            FormatOptions::Json => {
                let json = serde_json::to_string_pretty(&gen_changepack_result_map(
                    projects.as_slice(),
                    repo_root_path,
                    update_map,
                )?)?;
                println!("{}", json);
            }
        }
        Ok(())
    }
}
