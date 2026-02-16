use std::collections::BTreeMap;

use anyhow::Result;
use changepacks_core::PublishResult;
use changepacks_utils::sort_by_dependencies;
use clap::Args;

use crate::{
    CommandContext,
    options::FormatOptions,
    prompter::{InquirePrompter, Prompter},
};
use changepacks_core::Language;

#[derive(Args, Debug)]
#[command(about = "Publish packages")]
pub struct PublishArgs {
    #[arg(short, long)]
    pub dry_run: bool,

    #[arg(short, long)]
    pub yes: bool,

    #[arg(long, default_value = "stdout")]
    pub format: FormatOptions,

    #[arg(short, long, default_value = "false")]
    pub remote: bool,

    /// Filter projects by language. Can be specified multiple times to include multiple languages.
    #[arg(short, long, value_enum)]
    pub language: Vec<crate::options::CliLanguage>,

    /// Filter projects by relative path (e.g., packages/foo/package.json). Can be specified multiple times.
    #[arg(short, long)]
    pub project: Vec<String>,
}

/// Publish packages
///
/// # Errors
/// Returns error if command context creation or publishing fails.
pub async fn handle_publish(args: &PublishArgs) -> Result<()> {
    handle_publish_with_prompter(args, &InquirePrompter).await
}

/// # Errors
/// Returns error if project discovery, dependency sorting, or publishing fails.
pub async fn handle_publish_with_prompter(
    args: &PublishArgs,
    prompter: &dyn Prompter,
) -> Result<()> {
    let ctx = CommandContext::new(args.remote).await?;

    let mut projects: Vec<_> = ctx
        .project_finders
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
        for project in &projects {
            println!("  {project}");
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
        prompter.confirm("Are you sure you want to publish the packages?")?
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
    let mut failed_projects: Vec<String> = Vec::new();

    // Publish each project
    for project in &projects {
        if let FormatOptions::Stdout = args.format {
            println!("Publishing {project}...");
        }
        let result = project.publish(&ctx.config).await;
        match result {
            Ok(_) => {
                if let FormatOptions::Stdout = args.format {
                    println!("Successfully published {project}");
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
                    eprintln!("Failed to publish {project}: {e}");
                }
                if let FormatOptions::Json = args.format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(false, Some(e.to_string())),
                    );
                }
                failed_projects.push(format!("{project}"));
            }
        }
    }

    if !failed_projects.is_empty()
        && let FormatOptions::Stdout = args.format
    {
        eprintln!(
            "\n{} of {} projects failed to publish: {}",
            failed_projects.len(),
            projects.len(),
            failed_projects.join(", ")
        );
    }

    if let FormatOptions::Json = args.format {
        println!("{}", serde_json::to_string_pretty(&result_map)?);
    }

    if !failed_projects.is_empty() {
        anyhow::bail!(
            "Failed to publish {} project(s): {}",
            failed_projects.len(),
            failed_projects.join(", ")
        );
    }

    Ok(())
}

#[cfg(test)]
fn publish_result_from_failures(failed: &[String], total: usize) -> Result<()> {
    if !failed.is_empty() {
        anyhow::bail!(
            "Failed to publish {} of {} project(s): {}",
            failed.len(),
            total,
            failed.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        publish: PublishArgs,
    }

    #[test]
    fn test_publish_args_default() {
        let cli = TestCli::parse_from(["test"]);
        assert!(!cli.publish.dry_run);
        assert!(!cli.publish.yes);
        assert!(matches!(cli.publish.format, FormatOptions::Stdout));
        assert!(!cli.publish.remote);
        assert!(cli.publish.language.is_empty());
        assert!(cli.publish.project.is_empty());
    }

    #[test]
    fn test_publish_args_with_dry_run() {
        let cli = TestCli::parse_from(["test", "--dry-run"]);
        assert!(cli.publish.dry_run);
    }

    #[test]
    fn test_publish_args_with_yes() {
        let cli = TestCli::parse_from(["test", "--yes"]);
        assert!(cli.publish.yes);
    }

    #[test]
    fn test_publish_args_with_format_json() {
        let cli = TestCli::parse_from(["test", "--format", "json"]);
        assert!(matches!(cli.publish.format, FormatOptions::Json));
    }

    #[test]
    fn test_publish_args_with_remote() {
        let cli = TestCli::parse_from(["test", "--remote"]);
        assert!(cli.publish.remote);
    }

    #[test]
    fn test_publish_args_with_language_filter() {
        let cli = TestCli::parse_from(["test", "--language", "node"]);
        assert_eq!(cli.publish.language.len(), 1);
    }

    #[test]
    fn test_publish_args_with_multiple_languages() {
        let cli = TestCli::parse_from(["test", "--language", "node", "--language", "python"]);
        assert_eq!(cli.publish.language.len(), 2);
    }

    #[test]
    fn test_publish_args_with_project_filter() {
        let cli = TestCli::parse_from(["test", "--project", "packages/core/package.json"]);
        assert_eq!(cli.publish.project.len(), 1);
        assert_eq!(cli.publish.project[0], "packages/core/package.json");
    }

    #[test]
    fn test_publish_args_combined() {
        let cli = TestCli::parse_from([
            "test",
            "--dry-run",
            "--yes",
            "--format",
            "json",
            "--remote",
            "--language",
            "rust",
            "--project",
            "Cargo.toml",
        ]);
        assert!(cli.publish.dry_run);
        assert!(cli.publish.yes);
        assert!(matches!(cli.publish.format, FormatOptions::Json));
        assert!(cli.publish.remote);
        assert_eq!(cli.publish.language.len(), 1);
        assert_eq!(cli.publish.project.len(), 1);
    }

    #[test]
    fn test_publish_result_all_succeed() {
        let result = publish_result_from_failures(&[], 3);
        assert!(result.is_ok());
    }

    #[test]
    fn test_publish_result_single_failure() {
        let result = publish_result_from_failures(&["pkg-a".to_string()], 3);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("1 of 3"));
        assert!(err_msg.contains("pkg-a"));
    }

    #[test]
    fn test_publish_result_multiple_failures() {
        let result = publish_result_from_failures(&["pkg-a".to_string(), "pkg-b".to_string()], 5);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("2 of 5"));
        assert!(err_msg.contains("pkg-a"));
        assert!(err_msg.contains("pkg-b"));
    }
}
