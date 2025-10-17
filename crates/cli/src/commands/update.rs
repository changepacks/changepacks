use changepack_core::{UpdateLog, update_type::UpdateType};
use std::collections::HashMap;
use tokio::fs::{read_dir, read_to_string};

use anyhow::{Context, Result};
use clap::Args;
use utils::{display_project, find_current_git_repo, find_project_dirs, next_version};

use crate::finders::get_finders;

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct UpdateArgs {
    #[arg(short, long)]
    dry_run: bool,
}

/// Update project version
pub async fn handle_update(args: &UpdateArgs) -> Result<()> {
    let repo = find_current_git_repo()?;
    let changepack_dir = repo.workdir().unwrap().join(".changepack");
    // check if changepack.json exists
    let changepack_file = changepack_dir.join("changepack.json");
    if !changepack_file.exists() {
        return Err(anyhow::anyhow!("Changepack project not initialized"));
    }

    let mut update_map = HashMap::<String, UpdateType>::new();

    let mut entries = read_dir(&changepack_dir).await?;
    while let Some(file) = entries.next_entry().await? {
        if file.file_name().to_string_lossy() == "changepack.json" {
            continue;
        }
        let file_json = read_to_string(file.path()).await?;
        let file_json: UpdateLog = serde_json::from_str(&file_json)?;
        for (project_path, update_type) in file_json.changes().iter() {
            if update_map.contains_key(project_path) {
                if update_map[project_path] < *update_type {
                    update_map.insert(project_path.clone(), update_type.clone());
                }
                continue;
            }
            update_map.insert(project_path.clone(), update_type.clone());
        }
    }
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
            let update_type = update_map
                .get(project.path())
                .context(format!("Project not found: {}", project.path()))?;
            update_projects.push((project, update_type.clone()));
        }
    }
    update_projects.sort();
    for (project, update_type) in update_projects.iter() {
        println!(
            "{}: {} -> {}",
            display_project(project),
            update_type,
            next_version(project.version().unwrap(), update_type.clone())?
        );
    }
    if args.dry_run {
        println!("Dry run, no updates will be made");
        return Ok(());
    }
    // confirm
    let confirm =
        inquire::Confirm::new("Are you sure you want to update the projects?").prompt()?;
    if !confirm {
        println!("Update cancelled");
        return Ok(());
    }

    let futures = update_projects
        .into_iter()
        .map(|(project, update_type)| project.update_version(update_type.clone()));

    futures::future::join_all(futures).await;
    Ok(())
}
