use anyhow::Result;
use changepack_core::{project::Project, update_type::UpdateType};
use colored::*;

use crate::{get_relative_path::get_relative_path, next_version};

pub fn display_project(project: &Project, update_type: Option<UpdateType>) -> Result<String> {
    Ok(match project {
        Project::Workspace(workspace) => {
            let relative_path = get_relative_path(workspace.path())?;
            format!(
                "{} {} {} {} {}",
                format!("[Workspace - {}]", workspace.language())
                    .bright_blue()
                    .bold(),
                workspace.name().unwrap_or("unknown").bright_white().bold(),
                format!(
                    "({}{})",
                    workspace
                        .version()
                        .map(|v| format!("v{}", v))
                        .unwrap_or("unknown".to_string()),
                    update_type
                        .map(|t| next_version(workspace.version().unwrap_or("0.0.0"), t)
                            .map(|v| format!(" → v{}", v)))
                        .transpose()?
                        .unwrap_or("".to_string())
                )
                .bright_green(),
                "-".bright_cyan(),
                relative_path.bright_black()
            )
        }
        Project::Package(package) => {
            let relative_path = get_relative_path(package.path())?;
            format!(
                "{} {} {} {} {}",
                format!("[{}]", package.language()).bright_blue().bold(),
                package.name().bright_white().bold(),
                format!("(v{})", package.version()).bright_green(),
                "-".bright_cyan(),
                relative_path.bright_black()
            )
        }
    })
}
