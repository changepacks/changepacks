use colored::*;
use core::project::Project;
use std::path::PathBuf;

use crate::find_current_git_repo;

fn get_relative_path(absolute_path: &str) -> String {
    let git_repo = find_current_git_repo()
        .unwrap()
        .workdir()
        .unwrap()
        .to_path_buf();
    let git_root = git_repo.to_path_buf();
    let project_path = PathBuf::from(absolute_path);
    match project_path.strip_prefix(&git_root) {
        Ok(relative) => format!("./{}", relative.to_string_lossy()),
        Err(_) => absolute_path.to_string(),
    }
}

pub fn display_project(project: &Project) -> String {
    match project {
        Project::Workspace(workspace) => {
            let relative_path = get_relative_path(workspace.path());
            format!(
                "{} {} {} {} {}",
                format!("[Workspace - {}]", workspace.language())
                    .bright_blue()
                    .bold(),
                workspace.name().unwrap_or("unknown").bright_white().bold(),
                format!(
                    "({})",
                    workspace
                        .version()
                        .map(|v| format!("v{}", v))
                        .unwrap_or("unknown".to_string())
                )
                .bright_green(),
                "→".bright_cyan(),
                relative_path.bright_black()
            )
        }
        Project::Package(package) => {
            let relative_path = get_relative_path(package.path());
            format!(
                "{} {} {} {} {}",
                format!("[{}]", package.language()).bright_blue().bold(),
                package.name().bright_white().bold(),
                format!("(v{})", package.version()).bright_green(),
                "→".bright_cyan(),
                relative_path.bright_black()
            )
        }
    }
}
