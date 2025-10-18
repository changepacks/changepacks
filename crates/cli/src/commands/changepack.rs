use changepack_core::{UpdateLog, project::Project, update_type::UpdateType};
use std::collections::HashMap;
use tokio::fs::write;

use utils::{
    display_project, find_current_git_repo, find_project_dirs, get_changepack_dir,
    get_relative_path,
};

use anyhow::{Context, Result};

use crate::{finders::get_finders, options::FilterOptions};

#[derive(Debug)]
pub struct ChangepackArgs {
    pub filter: Option<FilterOptions>,
}

pub async fn handle_changepack(args: &ChangepackArgs) -> Result<()> {
    let mut project_finders = get_finders();

    // collect all projects
    let repo = find_current_git_repo()?;
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
    // workspace first
    projects.sort();

    let mut update_map = HashMap::<String, UpdateType>::new();

    for update_type in [UpdateType::Major, UpdateType::Minor, UpdateType::Patch] {
        let project_names = projects
            .iter()
            .map(|project| display_project(project, None))
            .collect::<Result<Vec<_>>>()?;

        let message = format!("Select projects to update for {}", update_type);
        // select project to update
        let mut selector = inquire::MultiSelect::new(&message, project_names);
        selector.page_size = 15;
        let selected_projects = selector.prompt()?;

        // remove selected projects from projects by index
        for project_name in selected_projects {
            let project = projects
                .iter()
                .find(|project| display_project(project, None).unwrap() == project_name)
                .context(format!("Project not found: {}", project_name))?;
            update_map.insert(get_relative_path(project.path())?, update_type.clone());
        }

        let project_with_relpath: Vec<_> = projects
            .iter()
            .map(|project| get_relative_path(project.path()).map(|rel| (project, rel)))
            .collect::<Result<Vec<_>>>()?;

        let keep_projects: Vec<_> = project_with_relpath
            .into_iter()
            .filter(|(_, rel_path)| !update_map.contains_key(rel_path))
            .map(|(project, _)| *project)
            .collect();

        projects = keep_projects;
    }

    if update_map.is_empty() {
        println!("No projects selected");
        return Ok(());
    }

    let notes = inquire::Text::new("write notes here").prompt()?;
    if notes.is_empty() {
        println!("Notes are empty");
        return Ok(());
    }
    let update_log = UpdateLog::new(update_map, notes);
    // random uuid
    let update_log_id = nanoid::nanoid!();
    let update_log_file = get_changepack_dir()?.join(format!("update_log_{}.json", update_log_id));
    write(update_log_file, serde_json::to_string(&update_log)?).await?;

    Ok(())
}
