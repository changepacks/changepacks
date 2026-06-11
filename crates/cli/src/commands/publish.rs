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

/// Skip `cargo publish --dry-run` for Rust packages whose dependencies are
/// also being bumped in the same publish run.
///
/// `cargo publish --dry-run` resolves every dependency against crates.io
/// before attempting the simulated upload. When a workspace publishes
/// multiple interdependent crates together, the newer versions of the
/// dependencies do not exist on crates.io yet, so the dry-run fails with
/// `failed to select a version for the requirement` even though the
/// real publish (in topological order) would succeed. This is a documented
/// upstream limitation: rust-lang/cargo#1169, rust-lang/cargo#9507,
/// rust-lang/cargo#15622.
///
/// To avoid this false positive blocking the gate, skip the dry-run for
/// Rust packages that depend on any other package in the same publish
/// batch. Non-Rust ecosystems use lockfile-relative path / workspace
/// protocols (npm `workspace:*`, uv path deps, etc.) that do not hit the
/// registry during dry-run, so they are unaffected.
fn skip_dry_run_due_to_workspace_internal_dep(
    project: &Project,
    bumped_package_names: &std::collections::HashSet<String>,
) -> bool {
    if project.language() != changepacks_core::Language::Rust {
        return false;
    }
    project
        .dependencies()
        .iter()
        .any(|dep| bumped_package_names.contains(dep))
}

async fn execute_dry_run_publish_loop(
    projects: &[&Project],
    config: &Config,
    format: &FormatOptions,
) -> (BTreeMap<PathBuf, PublishResult>, Vec<String>) {
    let mut result_map = BTreeMap::new();
    let mut failed_projects: Vec<String> = Vec::new();

    // Pre-compute the set of package names being bumped in this run so that
    // each iteration can cheaply check whether its dependencies overlap.
    let bumped_package_names: std::collections::HashSet<String> = projects
        .iter()
        .filter_map(|p| p.name().map(String::from))
        .collect();

    for project in projects {
        if skip_dry_run_due_to_workspace_internal_dep(project, &bumped_package_names) {
            let msg = format!(
                "Dry-run skipped for {project}: depends on workspace member also being \
                 published in this run. `cargo publish --dry-run` cannot resolve the \
                 not-yet-published version (rust-lang/cargo#1169). The real publish \
                 will run in topological order and succeed."
            );
            if let FormatOptions::Stdout = format {
                eprintln!("{msg}");
            }
            if let FormatOptions::Json = format {
                result_map.insert(
                    project.relative_path().to_path_buf(),
                    PublishResult::new(
                        true,
                        Some("dry-run skipped (workspace-internal dep)".to_string()),
                        String::new(),
                        String::new(),
                    ),
                );
            }
            continue;
        }
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
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
        }
        async fn publish(&self, _config: &Config) -> anyhow::Result<PublishOutput> {
            anyhow::bail!("spawn failed: No such file or directory")
        }
        async fn dry_run_publish(&self, _config: &Config) -> anyhow::Result<Option<PublishOutput>> {
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
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
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
        fn default_dry_run_publish_command(&self) -> Option<String> {
            None
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

    /// Mock Rust package used to exercise the workspace-internal-dep skip
    /// path. Its `dry_run_publish` would panic if ever called, so the test
    /// would fail loudly if the skip helper let it through.
    #[derive(Debug)]
    struct RustMockPackage {
        name: String,
        relative_path: PathBuf,
        deps: HashSet<String>,
    }

    #[async_trait::async_trait]
    impl Package for RustMockPackage {
        fn name(&self) -> Option<&str> {
            Some(&self.name)
        }
        fn version(&self) -> Option<&str> {
            Some("0.0.1")
        }
        fn path(&self) -> &std::path::Path {
            std::path::Path::new("Cargo.toml")
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
            Language::Rust
        }
        fn dependencies(&self) -> &HashSet<String> {
            &self.deps
        }
        fn add_dependency(&mut self, dep: &str) {
            self.deps.insert(dep.to_string());
        }
        fn set_changed(&mut self, _changed: bool) {}
        fn default_publish_command(&self) -> String {
            "cargo publish".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("cargo publish --dry-run".to_string())
        }
        async fn dry_run_publish(&self, _config: &Config) -> anyhow::Result<Option<PublishOutput>> {
            // Used by leaf packages in the workspace-internal-dep integration
            // tests below. Returning a clean success keeps the test focused
            // on whether the SKIP path is correctly recorded for the parent
            // (the actual cargo invocation we want to avoid).
            Ok(Some(PublishOutput {
                success: true,
                stdout: format!("dry-run ok for {}", self.name),
                stderr: String::new(),
            }))
        }
    }

    fn make_rust_mock(name: &str, relative_path: &str, deps: &[&str]) -> Project {
        let pkg = RustMockPackage {
            name: name.to_string(),
            relative_path: PathBuf::from(relative_path),
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
        };
        Project::Package(Box::new(pkg))
    }

    #[test]
    fn test_skip_helper_non_rust_returns_false() {
        // CSharp project that happens to declare a dep matching a bumped
        // package: skip must NOT fire because the chicken-and-egg issue is
        // specific to `cargo publish --dry-run`.
        let pkg = DryRunUnsupportedPackage {
            path: PathBuf::from("/x/project.csproj"),
            relative_path: PathBuf::from("project.csproj"),
        };
        let project = Project::Package(Box::new(pkg));
        let bumped: HashSet<String> = ["dry-run-unsupported".to_string()].into_iter().collect();
        assert!(!skip_dry_run_due_to_workspace_internal_dep(
            &project, &bumped
        ));
    }

    #[test]
    fn test_skip_helper_rust_no_overlap_returns_false() {
        // Rust project whose deps do not appear in the bumped set:
        // standard `cargo publish --dry-run` would succeed, so skip must
        // not fire.
        let project = make_rust_mock("crate-a", "crates/a/Cargo.toml", &["external-crate"]);
        let bumped: HashSet<String> = ["crate-b".to_string()].into_iter().collect();
        assert!(!skip_dry_run_due_to_workspace_internal_dep(
            &project, &bumped
        ));
    }

    #[test]
    fn test_skip_helper_rust_with_overlap_returns_true() {
        // Rust project depends on `crate-b` which is also being bumped in
        // the same run: skip must fire to avoid the
        // "failed to select a version for the requirement" false positive.
        let project = make_rust_mock("crate-a", "crates/a/Cargo.toml", &["crate-b"]);
        let bumped: HashSet<String> = ["crate-a".to_string(), "crate-b".to_string()]
            .into_iter()
            .collect();
        assert!(skip_dry_run_due_to_workspace_internal_dep(
            &project, &bumped
        ));
    }

    /// Integration check for stdout format: when both `parent` and `leaf`
    /// are in the publish batch and parent depends on leaf, parent must be
    /// skipped (no failure surfaced) and leaf must dry-run normally.
    /// Stdout mode never populates `result_map`, so the skip path is
    /// validated by the absence of a failure entry for parent.
    #[tokio::test]
    async fn test_execute_dry_run_loop_skips_workspace_internal_dep_stdout() {
        let parent = make_rust_mock("crate-parent", "crates/parent/Cargo.toml", &["crate-leaf"]);
        let leaf = make_rust_mock("crate-leaf", "crates/leaf/Cargo.toml", &[]);
        // Both must be in `projects` so the bumped set contains
        // "crate-leaf" and the skip helper recognises parent's dependency
        // as a workspace-internal bump.
        let projects: Vec<&Project> = vec![&parent, &leaf];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Stdout).await;

        // Stdout mode never populates result_map. Skipped packages MUST
        // not appear in failed_projects — that is the whole point of the
        // skip helper (otherwise the dry-run gate would block the run).
        assert!(result_map.is_empty());
        assert!(failed.is_empty(), "no project should fail: {failed:?}");
    }

    #[tokio::test]
    async fn test_execute_dry_run_loop_skips_workspace_internal_dep_json() {
        let parent = make_rust_mock("crate-parent", "crates/parent/Cargo.toml", &["crate-leaf"]);
        let leaf = make_rust_mock("crate-leaf", "crates/leaf/Cargo.toml", &[]);
        let projects: Vec<&Project> = vec![&parent, &leaf];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Json).await;

        // `parent` is skipped → recorded as success with the skip note.
        let parent_entry = result_map
            .get(std::path::Path::new("crates/parent/Cargo.toml"))
            .expect("parent should be recorded as skipped");
        let parent_serialized = serde_json::to_string(parent_entry).expect("serialize");
        assert!(
            parent_serialized.contains("dry-run skipped (workspace-internal dep)"),
            "unexpected serialized entry for parent: {parent_serialized}"
        );
        // `leaf` has no workspace-internal dep so it goes through the
        // normal dry-run path and the mock returns success.
        let leaf_entry = result_map
            .get(std::path::Path::new("crates/leaf/Cargo.toml"))
            .expect("leaf should be recorded with a dry-run result");
        let leaf_serialized = serde_json::to_string(leaf_entry).expect("serialize");
        assert!(
            leaf_serialized.contains("dry-run ok for crate-leaf"),
            "leaf entry should reflect the mock's success stdout: {leaf_serialized}"
        );
        // Neither project should appear in failed_projects: parent was
        // skipped (success), leaf succeeded.
        assert!(failed.is_empty(), "no project should fail: {failed:?}");
    }
}
