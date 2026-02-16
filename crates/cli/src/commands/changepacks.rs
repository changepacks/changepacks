use changepacks_core::{ChangePackLog, Project, UpdateType};
use std::{collections::HashMap, path::PathBuf};
use tokio::fs::write;

use changepacks_utils::{
    find_current_git_repo, find_project_dirs, get_changepacks_config, get_changepacks_dir,
    get_relative_path,
};

use anyhow::{Context, Result};

use crate::{
    finders::get_finders,
    options::FilterOptions,
    prompter::{InquirePrompter, Prompter},
};

#[derive(Debug)]
pub struct ChangepackArgs {
    pub filter: Option<FilterOptions>,
    pub remote: bool,
    pub yes: bool,
    pub message: Option<String>,
    pub update_type: Option<UpdateType>,
}

pub async fn handle_changepack(args: &ChangepackArgs) -> Result<()> {
    handle_changepack_with_prompter(args, &InquirePrompter).await
}

pub async fn handle_changepack_with_prompter(
    args: &ChangepackArgs,
    prompter: &dyn Prompter,
) -> Result<()> {
    let mut project_finders = get_finders();
    let current_dir = std::env::current_dir()?;

    // collect all projects
    let repo = find_current_git_repo(&current_dir)?;
    let repo_root_path = repo.work_dir().context("Not a working directory")?;
    let config = get_changepacks_config(&current_dir).await?;
    find_project_dirs(&repo, &mut project_finders, &config, args.remote).await?;

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

    for update_type in if let Some(update_type) = &args.update_type {
        vec![*update_type]
    } else {
        vec![UpdateType::Major, UpdateType::Minor, UpdateType::Patch]
    } {
        if projects.is_empty() {
            break;
        }

        let selected_projects = if args.yes {
            projects.clone()
        } else if update_type == UpdateType::Patch && projects.len() == 1 {
            vec![projects[0]]
        } else {
            let message = format!("Select projects to update for {update_type}");
            let defaults = projects
                .iter()
                .enumerate()
                .filter_map(|(index, project)| {
                    if project.is_changed() {
                        Some(index)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            prompter.multi_select(&message, projects.clone(), defaults)?
        };

        // remove selected projects from projects by index
        for project in selected_projects {
            update_map.insert(
                get_relative_path(repo_root_path, project.path())?,
                update_type,
            );
        }

        let project_with_relpath: Vec<_> = projects
            .iter()
            .map(|project| {
                get_relative_path(repo_root_path, project.path()).map(|rel| (project, rel))
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

    let notes = if let Some(message) = &args.message {
        message.clone()
    } else {
        prompter.text("write notes here")?
    };

    if notes.is_empty() {
        println!("Notes are empty");
        return Ok(());
    }
    let changepack_log = ChangePackLog::new(update_map, notes);
    // random uuid
    let changepack_log_id = nanoid::nanoid!();
    let changepack_log_file =
        get_changepacks_dir(&current_dir)?.join(format!("changepack_log_{changepack_log_id}.json"));
    write(changepack_log_file, serde_json::to_string(&changepack_log)?).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_changepack_args_debug() {
        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: true,
            message: Some("Test".to_string()),
            update_type: Some(UpdateType::Patch),
        };

        // Test Debug trait
        let debug_str = format!("{:?}", args);
        assert!(debug_str.contains("ChangepackArgs"));
    }

    #[test]
    fn test_changepack_args_with_filter() {
        let args = ChangepackArgs {
            filter: Some(FilterOptions::Package),
            remote: true,
            yes: false,
            message: None,
            update_type: None,
        };

        assert!(args.filter.is_some());
        assert!(args.remote);
        assert!(!args.yes);
        assert!(args.message.is_none());
        assert!(args.update_type.is_none());
    }

    #[test]
    fn test_changepack_args_workspace_filter() {
        let args = ChangepackArgs {
            filter: Some(FilterOptions::Workspace),
            remote: false,
            yes: true,
            message: Some("msg".to_string()),
            update_type: Some(UpdateType::Major),
        };

        assert!(matches!(args.filter, Some(FilterOptions::Workspace)));
        assert!(matches!(args.update_type, Some(UpdateType::Major)));
    }

    #[test]
    fn test_changepack_args_minor_update() {
        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: true,
            message: Some("feature".to_string()),
            update_type: Some(UpdateType::Minor),
        };

        assert!(matches!(args.update_type, Some(UpdateType::Minor)));
    }
}
