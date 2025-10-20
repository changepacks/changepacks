use std::pin::Pin;

use anyhow::Result;
use clap::Args;
use std::future::Future;
use utils::{
    clear_update_logs, display_project, find_current_git_repo, find_project_dirs, gen_update_map,
    get_changepack_dir, get_relative_path,
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
    let changepack_dir = get_changepack_dir(&current_dir)?;
    // check if changepack.json exists
    let changepack_file = changepack_dir.join("changepack.json");
    if !changepack_file.exists() {
        return Err(anyhow::anyhow!("Changepack project not initialized"));
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
                update_map.get(&get_relative_path(&current_dir, project.path())?)
            {
                update_projects.push((project, update_type.clone()));
                continue;
            }
        }
    }
    update_projects.sort();
    for (project, update_type) in update_projects.iter() {
        println!("{}", display_project(project, Some(update_type.clone()))?);
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

    let mut all_futures: Vec<Pin<Box<dyn Future<Output = Result<()>>>>> = Vec::new();

    // Add remove file futures
    all_futures.push(Box::pin(
        async move { clear_update_logs(&changepack_dir).await },
    ));

    // Add update futures
    for (project, update_type) in update_projects {
        all_futures.push(Box::pin(async move {
            project.update_version(update_type).await
        }));
    }

    futures::future::join_all(all_futures).await;

    Ok(())
}
