use changepacks_core::{ChangePackResultLog, Project, UpdateType};

use anyhow::Result;
use changepacks_utils::{
    apply_reverse_dependencies, display_update, gen_changepack_result_map, gen_update_map,
    get_relative_path,
};
use clap::Args;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::{
    CommandContext,
    options::{FilterOptions, FormatOptions},
};

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct CheckArgs {
    #[arg(short, long)]
    filter: Option<FilterOptions>,

    #[arg(long, default_value = "stdout")]
    format: FormatOptions,

    #[arg(short, long, default_value = "false")]
    remote: bool,

    #[arg(long)]
    tree: bool,
}

/// Check project status
///
/// # Errors
/// Returns error if command context creation or project checking fails.
pub async fn handle_check(args: &CheckArgs) -> Result<()> {
    let ctx = CommandContext::new(args.remote).await?;

    let mut projects = ctx
        .project_finders
        .iter()
        .flat_map(|finder| finder.projects())
        .collect::<Vec<_>>();
    if let Some(filter) = &args.filter {
        projects.retain(|p| filter.matches(p));
    }
    projects.sort();
    if let FormatOptions::Stdout = args.format {
        println!("Found {} projects", projects.len());
    }
    let mut update_map = gen_update_map(&CommandContext::current_dir()?, &ctx.config).await?;

    // Apply reverse dependency updates (workspace:* dependencies)
    apply_reverse_dependencies(&mut update_map, &projects, &ctx.repo_root_path);

    if args.tree {
        // Tree mode: show dependencies as a tree
        display_tree(&projects, &ctx.repo_root_path, &update_map)?;
    } else {
        match args.format {
            FormatOptions::Stdout => {
                use colored::Colorize;
                for project in projects {
                    let changed_marker = if project.is_changed() {
                        " (changed)".bright_yellow()
                    } else {
                        "".normal()
                    };
                    println!(
                        "{}",
                        format!("{project}{changed_marker}",).replace(
                            project.version().unwrap_or("unknown"),
                            &if let Some(update_type) = update_map
                                .get(&get_relative_path(&ctx.repo_root_path, project.path())?)
                            {
                                display_update(project.version(), update_type.0)?
                            } else {
                                project.version().unwrap_or("unknown").to_string()
                            },
                        ),
                    )
                }
            }
            FormatOptions::Json => {
                let json = serde_json::to_string_pretty(&gen_changepack_result_map(
                    projects.as_slice(),
                    &ctx.repo_root_path,
                    &mut update_map,
                )?)?;
                println!("{json}");
            }
        }
    }
    Ok(())
}

/// Display projects as a dependency tree
fn display_tree(
    projects: &[&Project],
    repo_root_path: &std::path::Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
) -> Result<()> {
    // Create a map from project relative_path to project
    let mut path_to_project: HashMap<String, &Project> = HashMap::new();
    for project in projects {
        path_to_project.insert(project.name().unwrap_or("noname").to_string(), project);
    }

    // Build reverse dependency graph: graph[dep] = list of projects that depend on dep
    // This way, dependencies appear as children in the tree
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots: HashSet<String> = HashSet::new();
    let mut has_dependencies: HashSet<String> = HashSet::new();

    for project in projects {
        let deps = project.dependencies();
        // Filter dependencies to only include monorepo projects
        let monorepo_deps: Vec<String> = deps
            .iter()
            .filter(|dep| path_to_project.contains_key(*dep))
            .cloned()
            .collect();

        if !monorepo_deps.is_empty() {
            graph.insert(
                project.name().unwrap_or("noname").to_string(),
                monorepo_deps.clone(),
            );
            for dep in &monorepo_deps {
                has_dependencies.insert(dep.clone());
            }
        }
    }

    // Root nodes are projects that are not dependencies of any other project
    for project in projects {
        if !has_dependencies.contains(project.name().unwrap_or("noname")) {
            roots.insert(project.name().unwrap_or("noname").to_string());
        }
    }

    // Sort roots for consistent output
    let mut sorted_roots: Vec<String> = roots.into_iter().collect();
    sorted_roots.sort();

    // Display tree starting from roots
    let mut visited: HashSet<String> = HashSet::new();
    let mut ctx = TreeContext {
        graph: &graph,
        path_to_project: &path_to_project,
        repo_root_path,
        update_map,
    };
    for (idx, root) in sorted_roots.iter().enumerate() {
        if let Some(project) = path_to_project.get(root) {
            let is_last = idx == sorted_roots.len() - 1;
            display_tree_node(project, &mut ctx, "", is_last, &mut visited)?;
        }
    }

    // Display projects that weren't part of the tree (orphaned nodes)
    for project in projects {
        if !visited.contains(project.name().unwrap_or("noname")) {
            println!(
                "{}",
                format_project_line(project, repo_root_path, update_map, &path_to_project)?
            );
        }
    }

    Ok(())
}

/// Context for tree display operations
struct TreeContext<'a> {
    graph: &'a HashMap<String, Vec<String>>,
    path_to_project: &'a HashMap<String, &'a Project>,
    repo_root_path: &'a Path,
    update_map: &'a HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
}

/// Display a single node in the tree
fn display_tree_node(
    project: &Project,
    ctx: &mut TreeContext,
    prefix: &str,
    is_last: bool,
    visited: &mut HashSet<String>,
) -> Result<()> {
    let project_name = project.name().unwrap_or("noname").to_string();
    let is_first_visit = !visited.contains(&project_name);
    if is_first_visit {
        visited.insert(project_name.clone());
    }

    // Only print the project line if this is the first time visiting it
    if is_first_visit {
        let connector = if is_last { "└── " } else { "├── " };
        println!(
            "{}{}{}",
            prefix,
            connector,
            format_project_line(
                project,
                ctx.repo_root_path,
                ctx.update_map,
                ctx.path_to_project
            )?
        );
    }

    // Always display dependencies, even if the node was already visited
    // This ensures all dependencies are shown in the tree
    if let Some(deps) = ctx.graph.get(&project_name) {
        let mut sorted_deps = deps.clone();
        sorted_deps.sort();
        for (idx, dep_name) in sorted_deps.iter().enumerate() {
            if let Some(dep_project) = ctx.path_to_project.get(dep_name) {
                let is_last_dep = idx == sorted_deps.len() - 1;
                let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
                // Use a separate visited set for dependencies to avoid infinite loops
                // but still show all dependencies
                if visited.contains(dep_name) {
                    // If already visited, just print it without recursion to avoid loops
                    let dep_connector = if is_last_dep {
                        "└── "
                    } else {
                        "├── "
                    };
                    println!(
                        "{}{}{}",
                        new_prefix,
                        dep_connector,
                        format_project_line(
                            dep_project,
                            ctx.repo_root_path,
                            ctx.update_map,
                            ctx.path_to_project
                        )?
                    );
                } else {
                    display_tree_node(dep_project, ctx, &new_prefix, is_last_dep, visited)?;
                }
            }
        }
    }

    Ok(())
}

/// Format a project line for display
fn format_project_line(
    project: &Project,
    repo_root_path: &std::path::Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    path_to_project: &HashMap<String, &Project>,
) -> Result<String> {
    use changepacks_utils::get_relative_path;
    use colored::Colorize;

    let relative_path = get_relative_path(repo_root_path, project.path())?;
    let version = if let Some(update_entry) = update_map.get(&relative_path) {
        changepacks_utils::display_update(project.version(), update_entry.0)?
    } else {
        project
            .version()
            .map_or_else(|| "unknown".to_string(), |v| format!("v{v}"))
    };

    let changed_marker = if project.is_changed() {
        " (changed)".bright_yellow()
    } else {
        "".normal()
    };

    // Only show dependencies that are in the monorepo (in path_to_project)
    let monorepo_deps: Vec<String> = project
        .dependencies()
        .iter()
        .filter(|dep| path_to_project.contains_key(*dep))
        .map(std::string::ToString::to_string)
        .collect();

    let deps_info = if monorepo_deps.is_empty() {
        "".normal()
    } else {
        format!(" [deps:\n        {}]", monorepo_deps.join("\n        ")).bright_black()
    };

    // Format similar to Project::Display but with version update and dependencies
    let base_format = match project {
        Project::Workspace(w) => format!(
            "{} {} {} {} {}",
            format!("[Workspace - {}]", w.language())
                .bright_blue()
                .bold(),
            w.name().unwrap_or("noname").bright_white().bold(),
            format!("({version})").bright_green(),
            "-".bright_cyan(),
            w.relative_path().display().to_string().bright_black()
        ),
        Project::Package(p) => format!(
            "{} {} {} {} {}",
            format!("[{}]", p.language()).bright_blue().bold(),
            p.name().unwrap_or("noname").bright_white().bold(),
            format!("({version})").bright_green(),
            "-".bright_cyan(),
            p.relative_path().display().to_string().bright_black()
        ),
    };

    Ok(format!("{base_format}{changed_marker}{deps_info}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // Test CheckArgs parsing via clap
    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        check: CheckArgs,
    }

    #[test]
    fn test_check_args_default() {
        let cli = TestCli::parse_from(["test"]);
        assert!(cli.check.filter.is_none());
        assert!(matches!(cli.check.format, FormatOptions::Stdout));
        assert!(!cli.check.remote);
        assert!(!cli.check.tree);
    }

    #[test]
    fn test_check_args_with_json_format() {
        let cli = TestCli::parse_from(["test", "--format", "json"]);
        assert!(matches!(cli.check.format, FormatOptions::Json));
    }

    #[test]
    fn test_check_args_with_tree() {
        let cli = TestCli::parse_from(["test", "--tree"]);
        assert!(cli.check.tree);
    }

    #[test]
    fn test_check_args_with_remote() {
        let cli = TestCli::parse_from(["test", "--remote"]);
        assert!(cli.check.remote);
    }

    #[test]
    fn test_check_args_with_filter_workspace() {
        let cli = TestCli::parse_from(["test", "--filter", "workspace"]);
        assert!(matches!(cli.check.filter, Some(FilterOptions::Workspace)));
    }

    #[test]
    fn test_check_args_with_filter_package() {
        let cli = TestCli::parse_from(["test", "--filter", "package"]);
        assert!(matches!(cli.check.filter, Some(FilterOptions::Package)));
    }

    #[test]
    fn test_check_args_combined() {
        let cli = TestCli::parse_from([
            "test", "--filter", "package", "--format", "json", "--tree", "--remote",
        ]);
        assert!(matches!(cli.check.filter, Some(FilterOptions::Package)));
        assert!(matches!(cli.check.format, FormatOptions::Json));
        assert!(cli.check.tree);
        assert!(cli.check.remote);
    }
}
