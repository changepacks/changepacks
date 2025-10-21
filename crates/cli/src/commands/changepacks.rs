use changepacks_core::{ChangePackLog, project::Project, update_type::UpdateType};
use std::{collections::HashMap, path::PathBuf};
use tokio::fs::write;

use utils::{find_current_git_repo, find_project_dirs, get_changepacks_dir, get_relative_path};

use anyhow::{Context, Result};

use crate::{finders::get_finders, options::FilterOptions};

#[derive(Debug)]
pub struct ChangepackArgs {
    pub filter: Option<FilterOptions>,
}

pub async fn handle_changepack(args: &ChangepackArgs) -> Result<()> {
    let mut project_finders = get_finders();
    let current_dir = std::env::current_dir()?;

    // collect all projects
    let repo = find_current_git_repo(&current_dir)?;
    let repo_root_path = repo.work_dir().context("Not a working directory")?;
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

    let mut update_map = HashMap::<PathBuf, UpdateType>::new();

    for update_type in [UpdateType::Major, UpdateType::Minor, UpdateType::Patch] {
        if projects.is_empty() {
            break;
        }
        let message = format!("Select projects to update for {}", update_type);
        // select project to update
        let mut selector = inquire::MultiSelect::new(&message, projects.clone());
        selector.page_size = 15;
        selector.default = Some(
            projects
                .iter()
                .enumerate()
                .filter_map(|(index, project)| {
                    if project.is_changed() {
                        Some(index)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
        );
        selector.scorer = &|_input, option, _string_value, _idx| -> Option<i64> {
            if option.is_changed() {
                Some(100)
            } else {
                Some(0)
            }
        };
        selector.formatter = &|option| {
            option
                .iter()
                .map(|o| format!("{}", o.value))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let selected_projects = selector.prompt()?;

        // remove selected projects from projects by index
        for project in selected_projects {
            update_map.insert(
                get_relative_path(repo_root_path, project.path())?,
                update_type.clone(),
            );
        }

        let project_with_relpath: Vec<_> = projects
            .iter()
            .map(|project| {
                get_relative_path(&current_dir, project.path()).map(|rel| (project, rel))
            })
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
    let changepack_log = ChangePackLog::new(update_map, notes);
    // random uuid
    let changepack_log_id = nanoid::nanoid!();
    let changepack_log_file = get_changepacks_dir(&current_dir)?
        .join(format!("changepack_log_{}.json", changepack_log_id));
    write(changepack_log_file, serde_json::to_string(&changepack_log)?).await?;

    Ok(())
}
