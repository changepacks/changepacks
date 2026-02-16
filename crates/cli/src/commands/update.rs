use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{
    ChangePackResultLog, Package, Project, ProjectFinder, UpdateType, Workspace,
};
use changepacks_utils::{
    apply_reverse_dependencies, clear_update_logs, display_update, find_project_dirs,
    gen_changepack_result_map, gen_update_map, get_changepacks_dir, get_relative_path,
};
use clap::Args;

use crate::{
    CommandContext,
    finders::get_finders,
    options::FormatOptions,
    prompter::{InquirePrompter, Prompter},
};

type UpdateProjectMut<'a> = (&'a mut Project, UpdateType);
type WorkspaceRef<'a> = &'a dyn Workspace;

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct UpdateArgs {
    #[arg(short, long)]
    pub dry_run: bool,

    #[arg(short, long)]
    pub yes: bool,

    #[arg(long, default_value = "stdout")]
    pub format: FormatOptions,

    #[arg(short, long, default_value = "false")]
    pub remote: bool,
}

/// Update project version
///
/// # Errors
/// Returns error if command context creation or version update fails.
pub async fn handle_update(args: &UpdateArgs) -> Result<()> {
    handle_update_with_prompter(args, &InquirePrompter).await
}

/// # Errors
/// Returns error if reading changepack logs, updating versions, or writing results fails.
pub async fn handle_update_with_prompter(args: &UpdateArgs, prompter: &dyn Prompter) -> Result<()> {
    let ctx = CommandContext::new(args.remote).await?;
    let changepacks_dir = get_changepacks_dir(&CommandContext::current_dir()?)?;
    let mut update_map = gen_update_map(&CommandContext::current_dir()?, &ctx.config).await?;

    let mut project_finders = ctx.project_finders;
    let mut all_finders = get_finders();

    // Need a second git repo reference for the all_finders, but since CommandContext already called find_project_dirs
    // we use an empty config for all_finders which won't filter anything
    let current_dir = CommandContext::current_dir()?;
    let repo = changepacks_utils::find_current_git_repo(&current_dir)?;
    find_project_dirs(
        &repo,
        &mut all_finders,
        &changepacks_core::Config::default(),
        args.remote,
    )
    .await?;

    // Apply reverse dependency updates (workspace:* dependencies)
    let all_projects: Vec<&Project> = all_finders
        .iter()
        .flat_map(|finder| finder.projects())
        .collect();
    apply_reverse_dependencies(&mut update_map, &all_projects, &ctx.repo_root_path);

    if update_map.is_empty() {
        args.format.print("No updates found", "{}");
        return Ok(());
    }

    if let FormatOptions::Stdout = args.format {
        println!("Updates found:");
    }

    let (mut update_projects, workspace_projects) = collect_update_projects(
        &mut project_finders,
        &all_finders,
        &update_map,
        &ctx.repo_root_path,
    )?;

    if let FormatOptions::Stdout = args.format {
        for (project, update_type) in &update_projects {
            println!(
                "{} {}",
                project,
                display_update(project.version(), *update_type)?
            );
        }
    }

    if args.dry_run {
        args.format.print("Dry run, no updates will be made", "{}");
        return Ok(());
    }

    // confirm
    let confirm = if args.yes {
        true
    } else {
        prompter.confirm("Are you sure you want to update the projects?")?
    };

    if !confirm {
        args.format.print("Update cancelled", "{}");
        return Ok(());
    }

    apply_updates(&mut update_projects, &workspace_projects).await?;
    drop(update_projects);

    if let FormatOptions::Json = args.format {
        println!(
            "{}",
            serde_json::to_string_pretty(&gen_changepack_result_map(
                project_finders
                    .iter()
                    .flat_map(|finder| finder.projects())
                    .collect::<Vec<_>>()
                    .as_slice(),
                &ctx.repo_root_path,
                &mut update_map,
            )?)?
        );
    }

    // Clear files
    clear_update_logs(&changepacks_dir).await?;

    Ok(())
}

fn collect_update_projects<'a>(
    project_finders: &'a mut [Box<dyn ProjectFinder>],
    all_finders: &'a [Box<dyn ProjectFinder>],
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    repo_root_path: &Path,
) -> Result<(Vec<UpdateProjectMut<'a>>, Vec<WorkspaceRef<'a>>)> {
    let mut update_projects = Vec::new();
    let mut workspace_projects = Vec::new();

    for finder in project_finders {
        for project in finder.projects_mut() {
            if let Some((update_type, _)) =
                update_map.get(&get_relative_path(repo_root_path, project.path())?)
            {
                update_projects.push((project, *update_type));
            }
        }
    }

    for finder in all_finders {
        for project in finder.projects() {
            if let Project::Workspace(workspace) = project {
                workspace_projects.push(workspace.as_ref());
            }
        }
    }

    update_projects.sort();
    Ok((update_projects, workspace_projects))
}

async fn apply_updates(
    update_projects: &mut [UpdateProjectMut<'_>],
    workspace_projects: &[WorkspaceRef<'_>],
) -> Result<()> {
    futures::future::join_all(
        update_projects
            .iter_mut()
            .map(|(project, update_type)| project.update_version(*update_type)),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;

    let projects: Vec<&dyn Package> = update_projects
        .iter()
        .filter_map(|(project, _)| {
            if let Project::Package(package) = project {
                Some(package.as_ref())
            } else {
                None
            }
        })
        .collect();

    futures::future::join_all(
        workspace_projects
            .iter()
            .map(|workspace| workspace.update_workspace_dependencies(&projects)),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        update: UpdateArgs,
    }

    #[test]
    fn test_update_args_default() {
        let cli = TestCli::parse_from(["test"]);
        assert!(!cli.update.dry_run);
        assert!(!cli.update.yes);
        assert!(matches!(cli.update.format, FormatOptions::Stdout));
        assert!(!cli.update.remote);
    }

    #[test]
    fn test_update_args_with_dry_run() {
        let cli = TestCli::parse_from(["test", "--dry-run"]);
        assert!(cli.update.dry_run);
    }

    #[test]
    fn test_update_args_with_yes() {
        let cli = TestCli::parse_from(["test", "--yes"]);
        assert!(cli.update.yes);
    }

    #[test]
    fn test_update_args_with_format_json() {
        let cli = TestCli::parse_from(["test", "--format", "json"]);
        assert!(matches!(cli.update.format, FormatOptions::Json));
    }

    #[test]
    fn test_update_args_with_remote() {
        let cli = TestCli::parse_from(["test", "--remote"]);
        assert!(cli.update.remote);
    }

    #[test]
    fn test_update_args_combined() {
        let cli =
            TestCli::parse_from(["test", "--dry-run", "--yes", "--format", "json", "--remote"]);
        assert!(cli.update.dry_run);
        assert!(cli.update.yes);
        assert!(matches!(cli.update.format, FormatOptions::Json));
        assert!(cli.update.remote);
    }

    #[test]
    fn test_update_args_short_dry_run() {
        let cli = TestCli::parse_from(["test", "-d"]);
        assert!(cli.update.dry_run);
    }

    #[test]
    fn test_update_args_short_yes() {
        let cli = TestCli::parse_from(["test", "-y"]);
        assert!(cli.update.yes);
    }

    #[test]
    fn test_update_args_short_remote() {
        let cli = TestCli::parse_from(["test", "-r"]);
        assert!(cli.update.remote);
    }

    #[test]
    fn test_update_args_all_short_flags() {
        let cli = TestCli::parse_from(["test", "-d", "-y", "-r"]);
        assert!(cli.update.dry_run);
        assert!(cli.update.yes);
        assert!(cli.update.remote);
    }
}
