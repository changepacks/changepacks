use core::{UpdateLog, update_type::UpdateType};
use std::{collections::HashMap, fs::write};

use utils::{display_project, find_current_git_repo, find_project_dirs};

use anyhow::Result;

use crate::finders::get_finders;

pub fn handle_changepack() -> Result<()> {
    let mut project_finders = get_finders();

    // collect all projects
    let repo = find_current_git_repo()?;
    find_project_dirs(&repo, &mut project_finders)?;

    let mut projects = project_finders
        .iter()
        .flat_map(|finder| finder.projects())
        .collect::<Vec<_>>();

    println!("Found {} projects", projects.len());
    // workspace first
    projects.sort();

    let mut update_map = HashMap::<String, UpdateType>::new();

    for update_type in [UpdateType::Major, UpdateType::Minor, UpdateType::Patch] {
        let project_names = projects
            .iter()
            .map(|project| display_project(project))
            .collect::<Vec<_>>();

        let message = format!("Select projects to update for {}", update_type);
        // select project to update
        let mut selector = inquire::MultiSelect::new(&message, project_names);
        selector.page_size = 15;
        let selected_projects = selector.prompt()?;

        // remove selected projects from projects by index
        for project_name in selected_projects {
            let project = projects
                .iter()
                .find(|project| display_project(project) == project_name)
                .unwrap();
            update_map.insert(project.path().to_string(), update_type.clone());
        }
        projects.retain(|project| !update_map.contains_key(project.path()));
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
    let update_log_file = repo
        .workdir()
        .unwrap()
        .join(format!(".changepack/update_log_{}.json", update_log_id));
    write(update_log_file, serde_json::to_string(&update_log)?)?;

    Ok(())
}
