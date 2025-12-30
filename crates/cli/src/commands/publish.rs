use std::collections::BTreeMap;

use anyhow::Result;
use changepacks_core::PublishResult;
use changepacks_utils::{
    find_current_git_repo, find_project_dirs, get_changepacks_config, sort_by_dependencies,
};
use clap::Args;

use crate::{finders::get_finders, options::FormatOptions};
use changepacks_core::Language;

#[derive(Args, Debug)]
#[command(about = "Publish packages")]
pub struct PublishArgs {
    #[arg(short, long)]
    dry_run: bool,

    #[arg(short, long)]
    yes: bool,

    #[arg(long, default_value = "stdout")]
    format: FormatOptions,

    #[arg(short, long, default_value = "false")]
    remote: bool,

    /// Filter projects by language. Can be specified multiple times to include multiple languages.
    #[arg(short, long, value_enum)]
    language: Vec<crate::options::CliLanguage>,

    /// Filter projects by relative path (e.g., packages/foo/package.json). Can be specified multiple times.
    #[arg(short, long)]
    project: Vec<String>,
}

/// Publish packages
pub async fn handle_publish(args: &PublishArgs) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let repo = find_current_git_repo(&current_dir)?;

    let config = get_changepacks_config(&current_dir).await?;
    let mut project_finders = get_finders();

    find_project_dirs(&repo, &mut project_finders, &config, args.remote).await?;

    let mut projects: Vec<_> = project_finders
        .iter()
        .flat_map(|finder| finder.projects())
        .collect();

    // Filter by language if specified
    if !args.language.is_empty() {
        let allowed_languages: Vec<Language> = args
            .language
            .iter()
            .map(|&lang| Language::from(lang))
            .collect();
        projects.retain(|project| allowed_languages.contains(&project.language()));
    }

    // Filter by project relative path if specified
    if !args.project.is_empty() {
        projects.retain(|project| {
            let relative_path = project.relative_path().to_string_lossy();
            let normalized_path = relative_path.replace('\\', "/");
            args.project.iter().any(|p| {
                // Normalize path separators for comparison
                let normalized_p = p.replace('\\', "/");
                normalized_path == normalized_p
            })
        });
    }

    // Sort projects by dependencies (no cloning, just reordering references)
    let projects = sort_by_dependencies(projects);

    if projects.is_empty() {
        match args.format {
            FormatOptions::Stdout => {
                println!("No projects found");
            }
            FormatOptions::Json => {
                println!("{{}}");
            }
        }
        return Ok(());
    }

    if let FormatOptions::Stdout = args.format {
        println!("Projects to publish:");
        for project in projects.iter() {
            println!("  {}", project);
        }
    }

    if args.dry_run {
        match args.format {
            FormatOptions::Stdout => {
                println!("Dry run, no packages will be published");
            }
            FormatOptions::Json => {
                println!("{{}}");
            }
        }
        return Ok(());
    }

    // confirm
    let confirm = if args.yes {
        true
    } else {
        inquire::Confirm::new("Are you sure you want to publish the packages?").prompt()?
    };
    if !confirm {
        match args.format {
            FormatOptions::Stdout => {
                println!("Publish cancelled");
            }
            FormatOptions::Json => {
                println!("{{}}");
            }
        }
        return Ok(());
    }

    let mut result_map = BTreeMap::new();

    // Publish each project
    for project in projects.iter() {
        if let FormatOptions::Stdout = args.format {
            println!("Publishing {}...", project);
        }
        let result = project.publish(&config).await;
        match result {
            Ok(_) => {
                if let FormatOptions::Stdout = args.format {
                    println!("Successfully published {}", project);
                }
                if let FormatOptions::Json = args.format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(true, None),
                    );
                }
            }
            Err(e) => {
                if let FormatOptions::Stdout = args.format {
                    eprintln!("Failed to publish {}: {}", project, e);
                }
                if let FormatOptions::Json = args.format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(false, Some(e.to_string())),
                    );
                }
            }
        }
    }

    if let FormatOptions::Json = args.format {
        println!("{}", serde_json::to_string_pretty(&result_map)?);
    }
    Ok(())
}
