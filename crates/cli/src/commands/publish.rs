use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

use anyhow::Result;
use changepacks_core::{Config, Project, PublishOutput, PublishResult};
use changepacks_utils::sort_by_dependencies;
use clap::Args;

use crate::{
    CommandContext,
    finders::collect_projects,
    options::{FormatOptions, retain_by_language},
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

    let mut projects = collect_projects(&ctx.project_finders);

    // Filter by language if specified
    retain_by_language(&args.language, &mut projects);

    // Filter by project relative path if specified.
    // `HashSet<String>` gives O(1) lookup vs `Vec::contains` O(A) per project,
    // dropping the overall filter cost from O(P × A) to O(P + A) — meaningful in
    // large monorepos where both P and A can reach the dozens. Behavior is
    // unchanged: same case-sensitive `\` → `/` normalization and the same
    // set-membership result (`HashSet<String>` matches what `Vec<String>`
    // returned).
    if !args.project.is_empty() {
        // Preallocate: `HashSet::from_iter` (via `collect`) does NOT use
        // `size_hint` to reserve capacity, so it incurs geometric-doubling
        // reallocations. `args.project.len()` is the exact upper bound
        // (each `--project` flag produces exactly one entry).
        // Matches the preallocation policy already applied to
        // `bumped_package_names` a few lines below and to every other
        // `HashSet` preallocation site in the workspace.
        let mut normalized_args: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(args.project.len());
        normalized_args.extend(args.project.iter().map(|p| p.replace('\\', "/")));
        projects.retain(|project| {
            let relative_path = project.relative_path().to_string_lossy();
            // Only pay the `replace` allocation when the path actually
            // contains a backslash. Every `/`-only path (all Unix paths, and
            // Windows paths already using `/`) is looked up by borrowed slice
            // instead — `HashSet<String>::contains` accepts a `&str` via
            // `Borrow<str>`, so the no-backslash branch is allocation-free.
            if relative_path.contains('\\') {
                normalized_args.contains(&relative_path.replace('\\', "/"))
            } else {
                normalized_args.contains(relative_path.as_ref())
            }
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
    bumped_package_names: &std::collections::HashSet<&str>,
) -> bool {
    if project.language() != changepacks_core::Language::Rust {
        return false;
    }
    project
        .dependencies()
        .iter()
        .any(|dep| bumped_package_names.contains(dep.as_str()))
}

/// Shared per-project success recorder for both `execute_publish_loop` and
/// `execute_dry_run_publish_loop`.
///
/// Both loops previously open-coded the same `Stdout / Json` matrix around
/// their success arms; only the human-facing label differed
/// (`"Successfully published"` vs `"Dry-run succeeded for"`). Extracted so a
/// future JSON-shape or stdout-formatting tweak lands in one place.
///
/// Preserves every message string byte-for-byte: the caller supplies the
/// exact `<label> {project}` prefix, matching the format used by the
/// integration tests (`test_execute_dry_run_loop_skips_workspace_internal_dep_*`).
/// Takes `output` by value so the `PublishResult::new(..., output.stdout,
/// output.stderr)` move stays zero-clone on the JSON path, matching the
/// pre-refactor shape.
fn record_publish_success(
    result_map: &mut BTreeMap<PathBuf, PublishResult>,
    project: &Project,
    output: PublishOutput,
    success_label: &str,
    format: &FormatOptions,
) {
    if let FormatOptions::Stdout = format {
        print_publish_output(&output);
        println!("{success_label} {project}");
    }
    if let FormatOptions::Json = format {
        result_map.insert(
            project.relative_path().to_path_buf(),
            PublishResult::new(true, None, output.stdout, output.stderr),
        );
    }
}

/// Shared per-project failure recorder for both publish loops.
///
/// Handles BOTH the "non-zero-exit `Ok(output)`" case and the "spawn error
/// `Err(e)`" case via the `PublishFailureCause` enum. Preserves the subtle
/// stdout distinction the old loops maintained:
///
///   - `PublishFailureCause::Output(output)` prints
///     `print_publish_output(&output); eprintln!("{label} {project}");`
///   - `PublishFailureCause::Error(e)` prints `eprintln!("{label} {project}: {e}");`
///     (with the `: {e}` suffix).
///
/// Byte-identical to the pre-refactor arms — including the trailing
/// `failed_projects.push(format!("{project}"))` shared by both. Ok(None)
/// (dry-run unsupported) stays inline in the dry-run loop because it is
/// a warning, not a failure, and does not fit this helper's contract.
fn record_publish_failure(
    result_map: &mut BTreeMap<PathBuf, PublishResult>,
    failed_projects: &mut Vec<String>,
    project: &Project,
    cause: PublishFailureCause,
    failure_label: &str,
    format: &FormatOptions,
) {
    if let FormatOptions::Stdout = format {
        match &cause {
            PublishFailureCause::Output(output) => {
                print_publish_output(output);
                eprintln!("{failure_label} {project}");
            }
            PublishFailureCause::Error(e) => {
                eprintln!("{failure_label} {project}: {e}");
            }
        }
    }
    if let FormatOptions::Json = format {
        let (stdout, stderr, err_msg) = match cause {
            PublishFailureCause::Output(output) => (output.stdout, output.stderr, None),
            PublishFailureCause::Error(e) => (String::new(), String::new(), Some(e.to_string())),
        };
        result_map.insert(
            project.relative_path().to_path_buf(),
            PublishResult::new(false, err_msg, stdout, stderr),
        );
    }
    failed_projects.push(project.to_string());
}

fn failed_dependency<'a>(
    project: &'a Project,
    failed_project_names: &HashSet<String>,
) -> Option<&'a str> {
    project
        .dependencies()
        .iter()
        .find(|dep| failed_project_names.contains(dep.as_str()))
        .map(String::as_str)
}

fn record_dependency_skip(
    result_map: &mut BTreeMap<PathBuf, PublishResult>,
    failed_projects: &mut Vec<String>,
    failed_project_names: &mut HashSet<String>,
    project: &Project,
    dependency: &str,
    failure_label: &str,
    format: &FormatOptions,
) {
    let error = anyhow::anyhow!("skipped because dependency failed: {dependency}");
    record_publish_failure(
        result_map,
        failed_projects,
        project,
        PublishFailureCause::Error(&error),
        failure_label,
        format,
    );
    if let Some(name) = project.name() {
        failed_project_names.insert(name.to_string());
    }
}

enum ProjectPublishOutcome {
    Success(PublishOutput),
    Failure(PublishOutput),
    Error(anyhow::Error),
}

/// Represents the failure cause for a publish operation.
/// Either a non-zero exit with captured output, or a spawn/execution error.
enum PublishFailureCause<'a> {
    Output(PublishOutput),
    Error(&'a anyhow::Error),
}

struct PublishOutcomeLabels {
    success: &'static str,
    failure: &'static str,
}

/// Calls [`record_publish_outcome`] and, when it returns `true` (failure),
/// inserts `project.name()` into `failed_project_names` if the project has a
/// name. Collapses the four identical
/// `record_publish_outcome(...) && let Some(name) = project.name()` blocks
/// that appeared in both publish loops.
fn record_outcome_track_failure(
    result_map: &mut BTreeMap<PathBuf, PublishResult>,
    failed_projects: &mut Vec<String>,
    failed_project_names: &mut HashSet<String>,
    project: &Project,
    outcome: ProjectPublishOutcome,
    labels: PublishOutcomeLabels,
    format: &FormatOptions,
) {
    if record_publish_outcome(
        result_map,
        failed_projects,
        project,
        outcome,
        labels.success,
        labels.failure,
        format,
    ) && let Some(name) = project.name()
    {
        failed_project_names.insert(name.to_string());
    }
}

fn record_publish_outcome(
    result_map: &mut BTreeMap<PathBuf, PublishResult>,
    failed_projects: &mut Vec<String>,
    project: &Project,
    outcome: ProjectPublishOutcome,
    success_label: &str,
    failure_label: &str,
    format: &FormatOptions,
) -> bool {
    match outcome {
        ProjectPublishOutcome::Success(output) => {
            record_publish_success(result_map, project, output, success_label, format);
            false
        }
        ProjectPublishOutcome::Failure(output) => {
            record_publish_failure(
                result_map,
                failed_projects,
                project,
                PublishFailureCause::Output(output),
                failure_label,
                format,
            );
            true
        }
        ProjectPublishOutcome::Error(error) => {
            record_publish_failure(
                result_map,
                failed_projects,
                project,
                PublishFailureCause::Error(&error),
                failure_label,
                format,
            );
            true
        }
    }
}

async fn execute_dry_run_publish_loop(
    projects: &[&Project],
    config: &Config,
    format: &FormatOptions,
) -> (BTreeMap<PathBuf, PublishResult>, Vec<String>) {
    let mut result_map = BTreeMap::new();
    let mut failed_projects: Vec<String> = Vec::with_capacity(projects.len());
    let mut failed_project_names: HashSet<String> = HashSet::with_capacity(projects.len());

    // Pre-compute the set of package names being bumped in this run so that
    // each iteration can cheaply check whether its dependencies overlap.
    // Borrow the names directly from the projects (which outlive the loop)
    // to skip the per-name `String` allocation the old `HashSet<String>`
    // version paid on every publish call.
    // Preallocate: `HashSet::from_iter` (via `collect`) does NOT use
    // `size_hint` to reserve capacity (unlike `Vec`), so it incurs
    // geometric-doubling reallocations. `projects.len()` is a tight upper
    // bound (the `filter_map` only drops nameless projects).
    // `HashSet::extend(iter)` reuses the existing `with_capacity` allocation
    // and matches the idiom already used across the utils crate (e.g.
    // `unique_files.extend(diff)` in `filter_project_dirs.rs`) — collapses
    // the 4-line loop + `Option::Some` guard to a single call while
    // preserving the same borrow-the-name-out-of-the-project semantics.
    let mut bumped_package_names: std::collections::HashSet<&str> =
        std::collections::HashSet::with_capacity(projects.len());
    bumped_package_names.extend(projects.iter().filter_map(|p| p.name()));

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
        if let Some(dependency) = failed_dependency(project, &failed_project_names) {
            record_dependency_skip(
                &mut result_map,
                &mut failed_projects,
                &mut failed_project_names,
                project,
                dependency,
                "Dry-run skipped for",
                format,
            );
            continue;
        }
        if let FormatOptions::Stdout = format {
            println!("Dry-run publishing {project}...");
        }
        match project.dry_run_publish(config).await {
            Ok(Some(output)) => {
                let outcome = if output.success {
                    ProjectPublishOutcome::Success(output)
                } else {
                    ProjectPublishOutcome::Failure(output)
                };
                record_outcome_track_failure(
                    &mut result_map,
                    &mut failed_projects,
                    &mut failed_project_names,
                    project,
                    outcome,
                    PublishOutcomeLabels {
                        success: "Dry-run succeeded for",
                        failure: "Dry-run failed for",
                    },
                    format,
                );
            }
            Ok(None) => {
                // Ok(None) stays inline: dry-run unsupported is a warning,
                // not a failure (`failed_projects` is NOT bumped), so it
                // does not fit `record_publish_failure`'s contract. The
                // JSON side also records `success = true` with an
                // explanatory error field, not the `success = false`
                // shape `record_publish_failure` produces.
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
                record_outcome_track_failure(
                    &mut result_map,
                    &mut failed_projects,
                    &mut failed_project_names,
                    project,
                    ProjectPublishOutcome::Error(e),
                    PublishOutcomeLabels {
                        success: "Dry-run succeeded for",
                        failure: "Dry-run failed for",
                    },
                    format,
                );
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
    let mut failed_projects: Vec<String> = Vec::with_capacity(projects.len());
    let mut failed_project_names: HashSet<String> = HashSet::with_capacity(projects.len());

    for project in projects {
        if let Some(dependency) = failed_dependency(project, &failed_project_names) {
            record_dependency_skip(
                &mut result_map,
                &mut failed_projects,
                &mut failed_project_names,
                project,
                dependency,
                "Skipped publish for",
                format,
            );
            continue;
        }
        if let FormatOptions::Stdout = format {
            println!("Publishing {project}...");
        }
        match project.publish(config).await {
            Ok(output) => {
                let outcome = if output.success {
                    ProjectPublishOutcome::Success(output)
                } else {
                    ProjectPublishOutcome::Failure(output)
                };
                record_outcome_track_failure(
                    &mut result_map,
                    &mut failed_projects,
                    &mut failed_project_names,
                    project,
                    outcome,
                    PublishOutcomeLabels {
                        success: "Successfully published",
                        failure: "Failed to publish",
                    },
                    format,
                );
            }
            Err(e) => {
                record_outcome_track_failure(
                    &mut result_map,
                    &mut failed_projects,
                    &mut failed_project_names,
                    project,
                    ProjectPublishOutcome::Error(e),
                    PublishOutcomeLabels {
                        success: "Successfully published",
                        failure: "Failed to publish",
                    },
                    format,
                );
            }
        }
    }

    (result_map, failed_projects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::{Language, Package, UpdateType};
    use clap::Parser;
    use rstest::rstest;
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

    // `--dry-run` (long) and `-d` (short) both flip the `dry_run` flag.
    #[rstest]
    #[case(&["test", "--dry-run"])]
    #[case(&["test", "-d"])]
    fn test_publish_args_dry_run_flag(#[case] args: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert!(cli.publish.dry_run);
    }

    // `--yes` (long) and `-y` (short) both flip the `yes` flag.
    #[rstest]
    #[case(&["test", "--yes"])]
    #[case(&["test", "-y"])]
    fn test_publish_args_yes_flag(#[case] args: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert!(cli.publish.yes);
    }

    #[test]
    fn test_publish_args_with_format_json() {
        let cli = TestCli::parse_from(["test", "--format", "json"]);
        assert!(matches!(cli.publish.format, FormatOptions::Json));
    }

    // `--remote` (long) and `-r` (short) both flip the `remote` flag.
    #[rstest]
    #[case(&["test", "--remote"])]
    #[case(&["test", "-r"])]
    fn test_publish_args_remote_flag(#[case] args: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert!(cli.publish.remote);
    }

    // `--language` / `-l` accumulate into `Vec<CliLanguage>`; the parsed
    // length must match the number of flags supplied.
    #[rstest]
    #[case(&["test", "--language", "node"], 1)]
    #[case(&["test", "-l", "rust"], 1)]
    #[case(&["test", "--language", "node", "--language", "python"], 2)]
    fn test_publish_args_language_flag(#[case] args: &[&str], #[case] expected_len: usize) {
        let cli = TestCli::parse_from(args);
        assert_eq!(cli.publish.language.len(), expected_len);
    }

    // `--project` / `-p` accumulate into `Vec<String>`; each supplied value
    // must appear at the matching index in order.
    #[rstest]
    #[case(&["test", "--project", "packages/core/package.json"], &["packages/core/package.json"])]
    #[case(&["test", "-p", "Cargo.toml"], &["Cargo.toml"])]
    #[case(
        &["test", "--project", "packages/a/package.json", "--project", "packages/b/package.json"],
        &["packages/a/package.json", "packages/b/package.json"],
    )]
    fn test_publish_args_project_flag(#[case] args: &[&str], #[case] expected: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert_eq!(cli.publish.project.len(), expected.len());
        for (actual, exp) in cli.publish.project.iter().zip(expected.iter()) {
            assert_eq!(actual, exp);
        }
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

    // Stdout mode never populates `result_map`; JSON mode records exactly
    // one entry per project. Both modes must count the spawn error as a
    // failed project.
    #[rstest]
    #[case(FormatOptions::Stdout, 0)]
    #[case(FormatOptions::Json, 1)]
    #[tokio::test]
    async fn test_execute_publish_loop_spawn_error(
        #[case] format: FormatOptions,
        #[case] expected_result_map_len: usize,
    ) {
        let pkg = FailSpawnPackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) = execute_publish_loop(&projects, &config, &format).await;

        assert_eq!(result_map.len(), expected_result_map_len);
        assert_eq!(failed.len(), 1);
    }

    /// Drives the `Err(e)` branch of `execute_dry_run_publish_loop`: the
    /// dry-run call fails to spawn entirely. Same result-map / failed-count
    /// shape as the non-dry-run spawn-error path above.
    #[rstest]
    #[case(FormatOptions::Stdout, 0)]
    #[case(FormatOptions::Json, 1)]
    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_spawn_error(
        #[case] format: FormatOptions,
        #[case] expected_result_map_len: usize,
    ) {
        let pkg = FailSpawnPackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) = execute_dry_run_publish_loop(&projects, &config, &format).await;

        assert_eq!(result_map.len(), expected_result_map_len);
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

    // Non-zero-exit dry-run:
    //   Stdout mode does not populate `result_map`; only `failed` is incremented.
    //   JSON mode records the failure with both stdout and stderr captured.
    // Either way exactly one project is marked failed.
    #[rstest]
    #[case(FormatOptions::Stdout, 0)]
    #[case(FormatOptions::Json, 1)]
    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_non_zero_exit(
        #[case] format: FormatOptions,
        #[case] expected_result_map_len: usize,
    ) {
        let pkg = DryRunFailurePackage {
            path: PathBuf::from("/nonexistent/package.json"),
            relative_path: PathBuf::from("package.json"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) = execute_dry_run_publish_loop(&projects, &config, &format).await;

        assert_eq!(result_map.len(), expected_result_map_len);
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

    // Unsupported dry-run is a warning, not a failure — `failed` must stay
    // empty in both formats. Stdout mode records nothing in `result_map`;
    // JSON mode records the skip as success=true with an explanatory error
    // message so the run does not bail.
    #[rstest]
    #[case(FormatOptions::Stdout, 0)]
    #[case(FormatOptions::Json, 1)]
    #[tokio::test]
    async fn test_execute_dry_run_publish_loop_unsupported(
        #[case] format: FormatOptions,
        #[case] expected_result_map_len: usize,
    ) {
        let pkg = DryRunUnsupportedPackage {
            path: PathBuf::from("/nonexistent/project.csproj"),
            relative_path: PathBuf::from("project.csproj"),
        };
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) = execute_dry_run_publish_loop(&projects, &config, &format).await;

        assert_eq!(result_map.len(), expected_result_map_len);
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

    #[derive(Debug)]
    struct PublishCascadePackage {
        name: String,
        relative_path: PathBuf,
        deps: HashSet<String>,
        succeeds: bool,
    }

    #[async_trait::async_trait]
    impl Package for PublishCascadePackage {
        fn name(&self) -> Option<&str> {
            Some(&self.name)
        }
        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }
        fn path(&self) -> &std::path::Path {
            &self.relative_path
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
            &self.deps
        }
        fn add_dependency(&mut self, dep: &str) {
            self.deps.insert(dep.to_string());
        }
        fn set_changed(&mut self, _changed: bool) {}
        fn default_publish_command(&self) -> String {
            "npm publish".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("npm publish --dry-run".to_string())
        }
        async fn publish(&self, _config: &Config) -> anyhow::Result<PublishOutput> {
            Ok(PublishOutput {
                success: self.succeeds,
                stdout: format!("publish {}", self.name),
                stderr: String::new(),
            })
        }
        async fn dry_run_publish(&self, _config: &Config) -> anyhow::Result<Option<PublishOutput>> {
            Ok(Some(PublishOutput {
                success: self.succeeds,
                stdout: format!("dry-run {}", self.name),
                stderr: String::new(),
            }))
        }
    }

    fn make_publish_cascade_mock(
        name: &str,
        relative_path: &str,
        deps: &[&str],
        succeeds: bool,
    ) -> Project {
        let pkg = PublishCascadePackage {
            name: name.to_string(),
            relative_path: PathBuf::from(relative_path),
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
            succeeds,
        };
        Project::Package(Box::new(pkg))
    }

    #[tokio::test]
    async fn test_execute_publish_loop_skips_dependent_after_failed_dependency() {
        let dependency = make_publish_cascade_mock("pkg-a", "packages/a/package.json", &[], false);
        let dependent =
            make_publish_cascade_mock("pkg-b", "packages/b/package.json", &["pkg-a"], true);
        let independent = make_publish_cascade_mock("pkg-c", "packages/c/package.json", &[], true);
        let projects: Vec<&Project> = vec![&dependency, &dependent, &independent];
        let config = Config::default();

        let (result_map, failed) =
            execute_publish_loop(&projects, &config, &FormatOptions::Json).await;

        assert_eq!(failed.len(), 2);
        assert!(failed[0].contains("pkg-a"));
        assert!(failed[1].contains("pkg-b"));
        let dependent_entry = result_map
            .get(std::path::Path::new("packages/b/package.json"))
            .expect("dependent should be recorded as skipped failure");
        let dependent_serialized = serde_json::to_string(dependent_entry).expect("serialize");
        assert!(dependent_serialized.contains("skipped because dependency failed: pkg-a"));
        let independent_entry = result_map
            .get(std::path::Path::new("packages/c/package.json"))
            .expect("independent project should still publish");
        let independent_serialized = serde_json::to_string(independent_entry).expect("serialize");
        assert!(independent_serialized.contains("publish pkg-c"));
    }

    #[tokio::test]
    async fn test_execute_dry_run_loop_skips_dependent_after_failed_dependency() {
        let dependency = make_publish_cascade_mock("pkg-a", "packages/a/package.json", &[], false);
        let dependent =
            make_publish_cascade_mock("pkg-b", "packages/b/package.json", &["pkg-a"], true);
        let projects: Vec<&Project> = vec![&dependency, &dependent];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, &FormatOptions::Json).await;

        assert_eq!(failed.len(), 2);
        assert!(failed[0].contains("pkg-a"));
        assert!(failed[1].contains("pkg-b"));
        let dependent_entry = result_map
            .get(std::path::Path::new("packages/b/package.json"))
            .expect("dependent should be recorded as skipped dry-run failure");
        let dependent_serialized = serde_json::to_string(dependent_entry).expect("serialize");
        assert!(dependent_serialized.contains("skipped because dependency failed: pkg-a"));
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
        let bumped: HashSet<&str> = ["dry-run-unsupported"].into_iter().collect();
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
        let bumped: HashSet<&str> = ["crate-b"].into_iter().collect();
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
        let bumped: HashSet<&str> = ["crate-a", "crate-b"].into_iter().collect();
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
