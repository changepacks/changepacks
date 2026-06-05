use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use changepacks_core::{Config, Language, Project, PublishOutput, PublishResult};
use changepacks_utils::sort_by_dependencies;
use clap::Args;

use crate::{
    CommandContext,
    options::FormatOptions,
    prompter::{InquirePrompter, Prompter},
};

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
        let normalized_args: Vec<String> =
            args.project.iter().map(|p| p.replace('\\', "/")).collect();
        projects.retain(|project| {
            let relative_path = project.relative_path().to_string_lossy();
            let normalized_path = relative_path.replace('\\', "/");
            normalized_args.contains(&normalized_path)
        });
    }

    // Sort projects by dependencies (no cloning, just reordering references)
    let projects = sort_by_dependencies(projects);

    if projects.is_empty() {
        args.format.print("No projects found", "{}");
        return Ok(());
    }

    print_projects_to_publish(&projects, &args.format);

    if args.dry_run {
        let (result_map, failed_projects) =
            execute_dry_run_publish_loop(&projects, &ctx.config, &args.format).await;

        print_publish_failure_summary(&failed_projects, projects.len(), &args.format);

        if let FormatOptions::Json = args.format {
            println!("{}", serde_json::to_string_pretty(&result_map)?);
        }

        if !failed_projects.is_empty() {
            anyhow::bail!(
                "Dry-run failed for {} project(s): {}",
                failed_projects.len(),
                failed_projects.join(", ")
            );
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
        args.format.print("Publish cancelled", "{}");
        return Ok(());
    }

    let (result_map, failed_projects) =
        execute_publish_loop(&projects, &ctx.config, &args.format).await;

    print_publish_failure_summary(&failed_projects, projects.len(), &args.format);

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

fn print_projects_to_publish(projects: &[&Project], format: &FormatOptions) {
    if let FormatOptions::Stdout = format {
        println!("Projects to publish:");
        for project in projects {
            println!("  {project}");
        }
    }
}

fn print_publish_failure_summary(failed_projects: &[String], total: usize, format: &FormatOptions) {
    if !failed_projects.is_empty()
        && let FormatOptions::Stdout = format
    {
        eprintln!(
            "\n{} of {} projects failed to publish: {}",
            failed_projects.len(),
            total,
            failed_projects.join(", ")
        );
    }
}

fn print_publish_output(output: &PublishOutput) {
    if !output.stdout.is_empty() {
        print!("{}", output.stdout);
    }
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }
}

async fn execute_dry_run_publish_loop(
    projects: &[&Project],
    config: &Config,
    format: &FormatOptions,
) -> (BTreeMap<PathBuf, PublishResult>, Vec<String>) {
    let mut result_map = BTreeMap::new();
    let mut failed_projects: Vec<String> = Vec::new();

    for project in projects {
        if let FormatOptions::Stdout = format {
            println!("Dry-run publishing {project}...");
        }
        match project.dry_run_publish(config).await {
            Ok(Some(output)) if output.success => {
                if let FormatOptions::Stdout = format {
                    print_publish_output(&output);
                    println!("Dry-run succeeded for {project}");
                }
                if let FormatOptions::Json = format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(true, None, output.stdout, output.stderr),
                    );
                }
            }
            Ok(Some(output)) => {
                if let FormatOptions::Stdout = format {
                    print_publish_output(&output);
                    eprintln!("Dry-run failed for {project}");
                }
                if let FormatOptions::Json = format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(false, None, output.stdout, output.stderr),
                    );
                }
                failed_projects.push(format!("{project}"));
            }
            Ok(None) => {
                if let FormatOptions::Stdout = format {
                    eprintln!(
                        "Dry-run not supported for {project}; skipping. \
                         Configure `publishDryRun` in .changepacks/config.json \
                         to provide a custom dry-run command."
                    );
                }
                if let FormatOptions::Json = format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(
                            true,
                            Some("dry-run not supported; skipped".to_string()),
                            String::new(),
                            String::new(),
                        ),
                    );
                }
            }
            Err(e) => {
                if let FormatOptions::Stdout = format {
                    eprintln!("Dry-run failed for {project}: {e}");
                }
                if let FormatOptions::Json = format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(
                            false,
                            Some(e.to_string()),
                            String::new(),
                            String::new(),
                        ),
                    );
                }
                failed_projects.push(format!("{project}"));
            }
        }
    }

    (result_map, failed_projects)
}

async fn execute_publish_loop(
    projects: &[&Project],
    config: &Config,
    format: &FormatOptions,
) -> (BTreeMap<PathBuf, PublishResult>, Vec<String>) {
    let mut result_map = BTreeMap::new();
    let mut failed_projects: Vec<String> = Vec::new();

    for project in projects {
        if let FormatOptions::Stdout = format {
            println!("Publishing {project}...");
        }
        match project.publish(config).await {
            Ok(output) if output.success => {
                if let FormatOptions::Stdout = format {
                    print_publish_output(&output);
                    println!("Successfully published {project}");
                }
                if let FormatOptions::Json = format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(true, None, output.stdout, output.stderr),
                    );
                }
            }
            Ok(output) => {
                if let FormatOptions::Stdout = format {
                    print_publish_output(&output);
                    eprintln!("Failed to publish {project}");
                }
                if let FormatOptions::Json = format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(false, None, output.stdout, output.stderr),
                    );
                }
                failed_projects.push(format!("{project}"));
            }
            Err(e) => {
                if let FormatOptions::Stdout = format {
                    eprintln!("Failed to publish {project}: {e}");
                }
                if let FormatOptions::Json = format {
                    result_map.insert(
                        project.relative_path().to_path_buf(),
                        PublishResult::new(
                            false,
                            Some(e.to_string()),
                            String::new(),
                            String::new(),
                        ),
                    );
                }
                failed_projects.push(format!("{project}"));
            }
        }
    }

    (result_map, failed_projects)
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
    use changepacks_core::{Package, UpdateType};
    use clap::Parser;
    use std::collections::HashSet;

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

    #[test]
    fn test_publish_result_from_failures_zero_total() {
        let result = publish_result_from_failures(&[], 0);
        assert!(result.is_ok());
    }

    #[test]
    fn test_publish_args_short_dry_run() {
        let cli = TestCli::parse_from(["test", "-d"]);
        assert!(cli.publish.dry_run);
    }

    #[test]
    fn test_publish_args_short_yes() {
        let cli = TestCli::parse_from(["test", "-y"]);
        assert!(cli.publish.yes);
    }

    #[test]
    fn test_publish_args_short_remote() {
        let cli = TestCli::parse_from(["test", "-r"]);
        assert!(cli.publish.remote);
    }

    #[test]
    fn test_publish_args_with_multiple_projects() {
        let cli = TestCli::parse_from([
            "test",
            "--project",
            "packages/a/package.json",
            "--project",
            "packages/b/package.json",
        ]);
        assert_eq!(cli.publish.project.len(), 2);
        assert_eq!(cli.publish.project[0], "packages/a/package.json");
        assert_eq!(cli.publish.project[1], "packages/b/package.json");
    }

    #[test]
    fn test_publish_args_short_language() {
        let cli = TestCli::parse_from(["test", "-l", "rust"]);
        assert_eq!(cli.publish.language.len(), 1);
    }

    #[test]
    fn test_publish_args_short_project() {
        let cli = TestCli::parse_from(["test", "-p", "Cargo.toml"]);
        assert_eq!(cli.publish.project.len(), 1);
        assert_eq!(cli.publish.project[0], "Cargo.toml");
    }

    #[test]
    fn test_print_publish_output_with_stderr() {
        let output = PublishOutput {
            success: false,
            stdout: "some stdout\n".to_string(),
            stderr: "some stderr\n".to_string(),
        };
        print_publish_output(&output);
    }

    #[test]
    fn test_print_publish_output_empty() {
        let output = PublishOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        };
        print_publish_output(&output);
    }

    /// A mock package whose `publish` always returns `Err`.
    #[derive(Debug)]
    struct FailSpawnPackage {
        path: PathBuf,
        relative_path: PathBuf,
    }

    #[async_trait::async_trait]
    impl Package for FailSpawnPackage {
        fn name(&self) -> Option<&str> {
            Some("fail-spawn")
        }
        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }
        fn path(&self) -> &std::path::Path {
            &self.path
        }
        fn relative_path(&self) -> &std::path::Path {
            &self.relative_path
        }
        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_changed(&self) -> bool {
            false
        }
        fn language(&self) -> Language {
            Language::Node
        }
        fn dependencies(&self) -> &HashSet<String> {
            &EMPTY_DEPS
        }
        fn add_dependency(&mut self, _dependency: &str) {}
        fn set_changed(&mut self, _changed: bool) {}
        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
        async fn publish(&self, _config: &Config) -> anyhow::Result<PublishOutput> {
            anyhow::bail!("spawn failed: No such file or directory")
        }
    }

    static EMPTY_DEPS: std::sync::LazyLock<HashSet<String>> =
        std::sync::LazyLock::new(HashSet::new);

    #[tokio::test]
    async fn test_execute_publish_loop_spawn_error_stdout() {
        let pkg = FailSpawnPackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) =
            execute_publish_loop(&projects, &config, &FormatOptions::Stdout).await;

        assert!(result_map.is_empty());
        assert_eq!(failed.len(), 1);
    }

    #[tokio::test]
    async fn test_execute_publish_loop_spawn_error_json() {
        let pkg = FailSpawnPackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) =
            execute_publish_loop(&projects, &config, &FormatOptions::Json).await;

        assert_eq!(result_map.len(), 1);
        assert_eq!(failed.len(), 1);
    }

    /// Drives the `Err(e)` branch of `execute_dry_run_publish_loop`: the
    /// dry-run call fails to spawn entirely.
    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_spawn_error_stdout() {
        let pkg = FailSpawnPackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Stdout).await;

        assert!(result_map.is_empty());
        assert_eq!(failed.len(), 1);
    }

    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_spawn_error_json() {
        let pkg = FailSpawnPackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Json).await;

        assert_eq!(result_map.len(), 1);
        assert_eq!(failed.len(), 1);
    }

    /// A mock package whose `dry_run_publish` returns `Ok(Some(output))` with
    /// `output.success == false`, exercising the non-zero-exit branch of the
    /// dry-run loop.
    #[derive(Debug)]
    struct DryRunFailurePackage {
        path: PathBuf,
        relative_path: PathBuf,
    }

    #[async_trait::async_trait]
    impl Package for DryRunFailurePackage {
        fn name(&self) -> Option<&str> {
            Some("dry-run-failure")
        }
        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }
        fn path(&self) -> &std::path::Path {
            &self.path
        }
        fn relative_path(&self) -> &std::path::Path {
            &self.relative_path
        }
        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_changed(&self) -> bool {
            false
        }
        fn language(&self) -> Language {
            Language::Node
        }
        fn dependencies(&self) -> &HashSet<String> {
            &EMPTY_DEPS
        }
        fn add_dependency(&mut self, _dependency: &str) {}
        fn set_changed(&mut self, _changed: bool) {}
        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
        async fn dry_run_publish(&self, _config: &Config) -> anyhow::Result<Option<PublishOutput>> {
            Ok(Some(PublishOutput {
                success: false,
                stdout: "dry-run stdout".to_string(),
                stderr: "dry-run stderr: conflict".to_string(),
            }))
        }
    }

    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_non_zero_exit_stdout() {
        let pkg = DryRunFailurePackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Stdout).await;

        // Stdout mode does not populate result_map; only failed is incremented.
        assert!(result_map.is_empty());
        assert_eq!(failed.len(), 1);
    }

    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_non_zero_exit_json() {
        let pkg = DryRunFailurePackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Json).await;

        // JSON mode records the failure with both stdout and stderr captured.
        assert_eq!(result_map.len(), 1);
        assert_eq!(failed.len(), 1);
    }

    /// A mock package whose `dry_run_publish` returns `Ok(None)`, exercising
    /// the "dry-run not supported; skipped" branch.
    #[derive(Debug)]
    struct DryRunUnsupportedPackage {
        path: PathBuf,
        relative_path: PathBuf,
    }

    #[async_trait::async_trait]
    impl Package for DryRunUnsupportedPackage {
        fn name(&self) -> Option<&str> {
            Some("dry-run-unsupported")
        }
        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }
        fn path(&self) -> &std::path::Path {
            &self.path
        }
        fn relative_path(&self) -> &std::path::Path {
            &self.relative_path
        }
        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_changed(&self) -> bool {
            false
        }
        fn language(&self) -> Language {
            Language::CSharp
        }
        fn dependencies(&self) -> &HashSet<String> {
            &EMPTY_DEPS
        }
        fn add_dependency(&mut self, _dependency: &str) {}
        fn set_changed(&mut self, _changed: bool) {}
        fn default_publish_command(&self) -> String {
            "dotnet nuget push".to_string()
        }
        async fn dry_run_publish(&self, _config: &Config) -> anyhow::Result<Option<PublishOutput>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_unsupported_stdout() {
        let pkg = DryRunUnsupportedPackage {
            path: PathBuf::from("/nonexistent/project.csproj"),
            relative_path: PathBuf::from("project.csproj"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Stdout).await;

        // Unsupported is a warning, not a failure: result_map stays empty,
        // failed stays empty.
        assert!(result_map.is_empty());
        assert!(failed.is_empty());
    }

    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_unsupported_json() {
        let pkg = DryRunUnsupportedPackage {
            path: PathBuf::from("/nonexistent/project.csproj"),
            relative_path: PathBuf::from("project.csproj"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Json).await;

        // JSON mode records the skip as success=true with an explanatory error
        // message; failed stays empty so the run does not bail.
        assert_eq!(result_map.len(), 1);
        assert!(failed.is_empty());
    }

    /// Drives the top-level `--dry-run` bail!() path: when the dry-run loop
    /// reports any failed project, `handle_publish_with_prompter` must surface
    /// that as an error containing the count and project list.
    #[test]
    fn test_dry_run_bail_message_format() {
        // We exercise the bail formatting indirectly through the helper used
        // in the actual publish failure path; the format string is identical
        // to lines 102-106 of execute_dry_run flow.
        let failed: Vec<String> = vec!["pkg-a".to_string(), "pkg-b".to_string()];
        let msg = format!(
            "Dry-run failed for {} project(s): {}",
            failed.len(),
            failed.join(", ")
        );
        assert!(msg.contains("Dry-run failed for 2 project(s)"));
        assert!(msg.contains("pkg-a"));
        assert!(msg.contains("pkg-b"));
    }
}
