use anyhow::{Context, Result};
use clap::Args;
use utils::{
    clear_update_logs, display_update, find_current_git_repo, find_project_dirs, gen_update_map,
    get_changepacks_dir, get_relative_path,
};

use crate::finders::get_finders;

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct UpdateArgs {
    #[arg(short, long)]
    dry_run: bool,

    #[arg(short, long)]
    yes: bool,
}

/// Update project version
pub async fn handle_update(args: &UpdateArgs) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let repo = find_current_git_repo(&current_dir)?;
    let repo_root_path = repo.work_dir().context("Not a working directory")?;
    let changepacks_dir = get_changepacks_dir(&current_dir)?;
    // check if changepacks.json exists
    let changepacks_file = changepacks_dir.join("changepacks.json");
    if !changepacks_file.exists() {
        return Err(anyhow::anyhow!("changepacks project not initialized"));
    }

    let update_map = gen_update_map(&current_dir).await?;

    if update_map.is_empty() {
        println!("No updates found");
        return Ok(());
    }
    println!("Updates found:");
    let mut finders = get_finders();

    find_project_dirs(&repo, &mut finders).await?;
    let mut update_projects = Vec::new();

    for finder in finders.iter_mut() {
        for project in finder.projects() {
            if let Some(update_type) =
                update_map.get(&get_relative_path(repo_root_path, project.path())?)
            {
                update_projects.push((project, update_type.clone()));
                continue;
            }
        }
    }
    update_projects.sort();
    for (project, update_type) in update_projects.iter() {
        println!(
            "{} {}",
            project,
            display_update(project.version(), update_type.clone())?
        );
    }
    if args.dry_run {
        println!("Dry run, no updates will be made");
        return Ok(());
    }
    // confirm
    let confirm = if args.yes {
        true
    } else {
        inquire::Confirm::new("Are you sure you want to update the projects?").prompt()?
    };
    if !confirm {
        println!("Update cancelled");
        return Ok(());
    }

    // Clear files
    clear_update_logs(&changepacks_dir).await?;

    futures::future::join_all(
        update_projects
            .iter()
            .map(|(project, update_type)| project.update_version(update_type.clone())),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;

    Ok(())
}
