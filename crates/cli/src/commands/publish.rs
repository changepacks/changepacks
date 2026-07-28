use std::{
    collections::{BTreeMap, HashSet},
    io::Write as _,
    path::PathBuf,
};

use anyhow::Result;
use changepacks_core::{Config, Project, PublishOutput, PublishResult, normalize_path_separators};
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

    #[arg(short, long)]
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
        // `rust_batch_names` a few lines below and to every other
        // `HashSet` preallocation site in the workspace.
        let mut normalized_args: HashSet<String> = HashSet::with_capacity(args.project.len());
        normalized_args.extend(
            args.project
                .iter()
                .map(|p| normalize_path_separators(p).into_owned()),
        );
        projects.retain(|project| {
            let relative_path = project.relative_path().to_string_lossy();
            // `normalize_path_separators` only allocates when the path actually
            // contains a backslash. Every `/`-only path (all Unix paths, and
            // Windows paths already using `/`) comes back as a borrowed slice,
            // and `HashSet<String>::contains` accepts a `&str` via
            // `Borrow<str>`, so the no-backslash lookup is allocation-free.
            normalized_args.contains(normalize_path_separators(&relative_path).as_ref())
        });
    }

    // Default-nonpublishable aggregate roots must leave the graph before the
    // mandatory topological sort. An explicit command for this publish mode
    // remains authoritative and keeps the project in the batch.
    let projects = sort_publishable_projects(projects, &ctx.config, args.dry_run)?;

    if projects.is_empty() {
        args.format.print("No projects found");
        return Ok(());
    }

    print_projects_to_publish(&projects, args.format)?;

    if args.dry_run {
        let (result_map, failed_projects) =
            execute_dry_run_publish_loop(&projects, &ctx.config, args.format).await;

        return finish_publish_run(
            &result_map,
            &failed_projects,
            projects.len(),
            args.format,
            "Dry-run failed for",
        );
    }

    // confirm
    let confirm =
        prompter.confirm_unless(args.yes, "Are you sure you want to publish the packages?")?;
    if !confirm {
        args.format.print("Publish cancelled");
        return Ok(());
    }

    let (result_map, failed_projects) =
        execute_publish_loop(&projects, &ctx.config, args.format).await;

    finish_publish_run(
        &result_map,
        &failed_projects,
        projects.len(),
        args.format,
        "Failed to publish",
    )
}

fn sort_publishable_projects<'a>(
    mut projects: Vec<&'a Project>,
    config: &Config,
    dry_run: bool,
) -> Result<Vec<&'a Project>> {
    let configured_commands = if dry_run {
        &config.publish_dry_run
    } else {
        &config.publish
    };
    projects.retain(|project| {
        (if dry_run {
            project.is_dry_run_publishable_by_default()
        } else {
            project.is_publishable_by_default()
        }) || changepacks_core::publish::lookup_by_path_or_language(
            configured_commands,
            project.relative_path(),
            project.language(),
        )
        .is_some()
    });
    Ok(sort_by_dependencies(projects)?)
}

/// Renders the "Projects to publish:" header and one indented line per project.
///
/// One stdout lock is held for the whole render: `println!` re-acquires the
/// global lock per line and panics on a write failure (a broken pipe from
/// `changepacks publish | head`), while a held `StdoutLock` writes through the
/// same `LineWriter` and lets an io error propagate as a typed error.
///
/// # Errors
/// Returns an error if writing to stdout fails.
fn print_projects_to_publish(projects: &[&Project], format: FormatOptions) -> Result<()> {
    if let FormatOptions::Stdout = format {
        let mut out = std::io::stdout().lock();
        writeln!(out, "Projects to publish:")?;
        for project in projects {
            writeln!(out, "  {project}")?;
        }
    }
    Ok(())
}

fn print_publish_failure_summary(failed_projects: &[String], total: usize, format: FormatOptions) {
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

/// Performs the shared finish tail for both the dry-run and real-publish branches:
/// prints the failure summary, emits the JSON result map when the format is JSON,
/// and bails with `"{bail_prefix} N project(s): list"` when there are failures.
///
/// `bail_prefix` must be `"Dry-run failed for"` (dry-run branch) or
/// `"Failed to publish"` (real-publish branch) — the exact strings pinned by
/// integration tests.
fn finish_publish_run(
    result_map: &BTreeMap<PathBuf, PublishResult>,
    failed_projects: &[String],
    total: usize,
    format: FormatOptions,
    bail_prefix: &str,
) -> Result<()> {
    print_publish_failure_summary(failed_projects, total, format);

    if let FormatOptions::Json = format {
        println!("{}", serde_json::to_string_pretty(result_map)?);
    }

    if !failed_projects.is_empty() {
        anyhow::bail!(
            "{} {} project(s): {}",
            bail_prefix,
            failed_projects.len(),
            failed_projects.join(", ")
        );
    }

    Ok(())
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
    rust_batch_names: &HashSet<&str>,
) -> bool {
    if project.language() != changepacks_core::Language::Rust {
        return false;
    }
    project
        .dependencies()
        .iter()
        .any(|dep| rust_batch_names.contains(dep.as_str()))
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
    format: FormatOptions,
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
/// `failed_projects.push(project.to_string())` shared by both. Ok(None)
/// (dry-run unsupported) stays inline in the dry-run loop because it is
/// a warning, not a failure, and does not fit this helper's contract.
fn record_publish_failure(
    result_map: &mut BTreeMap<PathBuf, PublishResult>,
    failed_projects: &mut Vec<String>,
    project: &Project,
    cause: PublishFailureCause,
    failure_label: &str,
    format: FormatOptions,
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
        .filter(|dep| failed_project_names.contains(dep.as_str()))
        .min()
        .map(String::as_str)
}

/// Records a dry-run skip as a JSON success entry with the provided note.
/// For JSON format, inserts a `PublishResult` with `success = true`, the given
/// `note` as the error message, and empty stdout/stderr. For Stdout format,
/// does nothing (the caller handles stdout output separately).
fn record_json_skip(
    result_map: &mut BTreeMap<PathBuf, PublishResult>,
    project: &Project,
    note: &str,
    format: FormatOptions,
) {
    if let FormatOptions::Json = format {
        result_map.insert(
            project.relative_path().to_path_buf(),
            PublishResult::new(true, Some(note.to_string()), String::new(), String::new()),
        );
    }
}

enum ProjectPublishOutcome {
    Success(PublishOutput),
    Failure(PublishOutput),
    Error(anyhow::Error),
}

impl ProjectPublishOutcome {
    fn from_output(output: PublishOutput) -> Self {
        if output.success {
            Self::Success(output)
        } else {
            Self::Failure(output)
        }
    }
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

/// The three collections both publish loops carry from the first project to
/// the last: the per-project JSON results, the ordered list of failed project
/// display names, and the set of failed project *package* names consulted by
/// `failed_dependency`.
///
/// Bundling them keeps the invariant "a project recorded in `failed_projects`
/// is also recorded in `failed_project_names`" expressible in one place —
/// `track_failed_name` — instead of once per loop. `result_map` starts
/// empty (a `BTreeMap` has no meaningful pre-sizing); the two others are
/// pre-sized to the batch length exactly as the loops did inline.
struct PublishLoopState {
    result_map: BTreeMap<PathBuf, PublishResult>,
    failed_projects: Vec<String>,
    failed_project_names: HashSet<String>,
}

impl PublishLoopState {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            result_map: BTreeMap::new(),
            failed_projects: Vec::with_capacity(capacity),
            failed_project_names: HashSet::with_capacity(capacity),
        }
    }

    /// Tracks `project`'s package name as failed so `failed_dependency` skips
    /// its dependents. Nameless projects contribute nothing to the set and are
    /// ignored. This is the single place that upholds the invariant documented
    /// on `PublishLoopState`: every project appended to `failed_projects` also
    /// has its package name recorded here.
    fn track_failed_name(&mut self, project: &Project) {
        if let Some(name) = project.name() {
            self.failed_project_names.insert(name.to_string());
        }
    }

    /// Records `outcome` into `result_map` / `failed_projects` via
    /// `record_publish_success` / `record_publish_failure`, then — on failure —
    /// tracks the project's package name via `Self::track_failed_name`.
    /// Collapses the four identical "record the outcome, then track its
    /// name on failure" blocks that appeared in both publish loops.
    fn record_outcome_track_failure(
        &mut self,
        project: &Project,
        outcome: ProjectPublishOutcome,
        labels: PublishOutcomeLabels,
        format: FormatOptions,
    ) {
        let failed = match outcome {
            ProjectPublishOutcome::Success(output) => {
                record_publish_success(
                    &mut self.result_map,
                    project,
                    output,
                    labels.success,
                    format,
                );
                false
            }
            ProjectPublishOutcome::Failure(output) => {
                record_publish_failure(
                    &mut self.result_map,
                    &mut self.failed_projects,
                    project,
                    PublishFailureCause::Output(output),
                    labels.failure,
                    format,
                );
                true
            }
            ProjectPublishOutcome::Error(error) => {
                record_publish_failure(
                    &mut self.result_map,
                    &mut self.failed_projects,
                    project,
                    PublishFailureCause::Error(&error),
                    labels.failure,
                    format,
                );
                true
            }
        };
        if failed {
            self.track_failed_name(project);
        }
    }

    /// Records `project` as failed-by-association with an already-failed
    /// `dependency`, then tracks its package name so its own dependents are
    /// skipped in turn.
    fn record_dependency_skip(
        &mut self,
        project: &Project,
        dependency: &str,
        failure_label: &str,
        format: FormatOptions,
    ) {
        let error = anyhow::anyhow!("skipped because dependency failed: {dependency}");
        record_publish_failure(
            &mut self.result_map,
            &mut self.failed_projects,
            project,
            PublishFailureCause::Error(&error),
            failure_label,
            format,
        );
        self.track_failed_name(project);
    }

    /// Records `project` as skipped when any of its dependencies already
    /// failed, returning `true` when the caller should `continue`.
    ///
    /// `failure_label` is the only thing that differed between the two loops
    /// (`"Dry-run skipped for"` vs `"Skipped publish for"`) and is passed
    /// through to `Self::record_dependency_skip` unchanged, so every
    /// user-visible string stays byte-identical.
    fn skip_if_dependency_failed(
        &mut self,
        project: &Project,
        failure_label: &str,
        format: FormatOptions,
    ) -> bool {
        let Some(dependency) = failed_dependency(project, &self.failed_project_names) else {
            return false;
        };
        // `failed_dependency` returns a `&str` borrowed from `project`, not
        // from `self.failed_project_names`, so the shared borrow of `self`
        // ends at the call and `&mut self` is free here.
        self.record_dependency_skip(project, dependency, failure_label, format);
        true
    }

    fn finish(self) -> (BTreeMap<PathBuf, PublishResult>, Vec<String>) {
        (self.result_map, self.failed_projects)
    }
}

async fn execute_dry_run_publish_loop(
    projects: &[&Project],
    config: &Config,
    format: FormatOptions,
) -> (BTreeMap<PathBuf, PublishResult>, Vec<String>) {
    let mut state = PublishLoopState::with_capacity(projects.len());

    const DRY_RUN_LABELS: PublishOutcomeLabels = PublishOutcomeLabels {
        success: "Dry-run succeeded for",
        failure: "Dry-run failed for",
    };

    // Names of ALL Rust projects in the current publish batch (not just
    // version-bumped ones — no bump information is consulted here), consulted
    // solely by `skip_dry_run_due_to_workspace_internal_dep` (which returns
    // `false` for all non-Rust projects): the skip only guards against
    // `cargo publish --dry-run` failing to resolve a not-yet-published *Rust*
    // workspace crate, so a non-Rust project that merely shares a name with a
    // Rust crate's dependency must not land in this set. Names are borrowed
    // from the projects, which outlive the loop.
    let mut rust_batch_names: HashSet<&str> = HashSet::with_capacity(projects.len());
    rust_batch_names.extend(
        projects
            .iter()
            .filter(|p| p.language() == changepacks_core::Language::Rust)
            .filter_map(|p| p.name()),
    );

    for project in projects {
        if state.skip_if_dependency_failed(project, "Dry-run skipped for", format) {
            continue;
        }
        if skip_dry_run_due_to_workspace_internal_dep(project, &rust_batch_names) {
            if let FormatOptions::Stdout = format {
                eprintln!(
                    "Dry-run skipped for {project}: depends on workspace member also being \
                     published in this run. `cargo publish --dry-run` cannot resolve the \
                     not-yet-published version (rust-lang/cargo#1169). The real publish \
                     will run in topological order and succeed."
                );
            }
            record_json_skip(
                &mut state.result_map,
                project,
                "dry-run skipped (workspace-internal dep)",
                format,
            );
            continue;
        }
        if let FormatOptions::Stdout = format {
            println!("Dry-run publishing {project}...");
        }
        match project.dry_run_publish(config).await {
            Ok(Some(output)) => {
                let outcome = ProjectPublishOutcome::from_output(output);
                state.record_outcome_track_failure(project, outcome, DRY_RUN_LABELS, format);
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
                record_json_skip(
                    &mut state.result_map,
                    project,
                    "dry-run not supported; skipped",
                    format,
                );
            }
            Err(e) => {
                state.record_outcome_track_failure(
                    project,
                    ProjectPublishOutcome::Error(e),
                    DRY_RUN_LABELS,
                    format,
                );
            }
        }
    }

    state.finish()
}

async fn execute_publish_loop(
    projects: &[&Project],
    config: &Config,
    format: FormatOptions,
) -> (BTreeMap<PathBuf, PublishResult>, Vec<String>) {
    let mut state = PublishLoopState::with_capacity(projects.len());

    const PUBLISH_LABELS: PublishOutcomeLabels = PublishOutcomeLabels {
        success: "Successfully published",
        failure: "Failed to publish",
    };

    for project in projects {
        if state.skip_if_dependency_failed(project, "Skipped publish for", format) {
            continue;
        }
        if let FormatOptions::Stdout = format {
            println!("Publishing {project}...");
        }
        match project.publish(config).await {
            Ok(output) => {
                let outcome = ProjectPublishOutcome::from_output(output);
                state.record_outcome_track_failure(project, outcome, PUBLISH_LABELS, format);
            }
            Err(e) => {
                state.record_outcome_track_failure(
                    project,
                    ProjectPublishOutcome::Error(e),
                    PUBLISH_LABELS,
                    format,
                );
            }
        }
    }

    state.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::{Language, Package, ProjectFinder, UpdateType, Workspace};
    use changepacks_csharp::CSharpProjectFinder;
    use changepacks_dart::DartProjectFinder;
    use changepacks_java::package::GradlePackage;
    use changepacks_node::NodeProjectFinder;
    use changepacks_python::PythonProjectFinder;
    use changepacks_rust::RustProjectFinder;
    use changepacks_utils::test_support::{DirGuard, git_add_and_commit, init_git_repo};
    use clap::Parser;
    use rstest::rstest;
    use serial_test::serial;
    use std::{
        collections::HashSet,
        path::Path,
        sync::{Arc, Mutex},
    };
    use tempfile::tempdir;

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

    #[derive(Debug)]
    struct TestWorkspace {
        label: String,
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        is_changed: bool,
        dependencies: HashSet<String>,
        publishable_by_default: bool,
        publish_log: Arc<Mutex<Vec<String>>>,
    }

    impl TestWorkspace {
        fn recording_rust(
            label: &str,
            name: Option<&str>,
            relative_path: &str,
            dependencies: &[&str],
            publishable_by_default: bool,
            publish_log: Arc<Mutex<Vec<String>>>,
        ) -> Self {
            Self {
                label: label.to_string(),
                name: name.map(str::to_string),
                version: Some("1.0.0".to_string()),
                path: PathBuf::from(relative_path),
                relative_path: PathBuf::from(relative_path),
                is_changed: false,
                dependencies: dependencies
                    .iter()
                    .map(|dependency| (*dependency).to_string())
                    .collect(),
                publishable_by_default,
                publish_log,
            }
        }
    }

    #[async_trait::async_trait]
    impl Workspace for TestWorkspace {
        changepacks_core::impl_basic_accessors!();

        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }

        changepacks_core::impl_language!(Language::Rust);
        changepacks_core::impl_dependencies_accessors!();

        fn is_publishable_by_default(&self) -> bool {
            self.publishable_by_default
        }

        fn default_publish_command(&self) -> String {
            "cargo publish".to_string()
        }

        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("cargo publish --dry-run".to_string())
        }

        async fn publish(&self, _config: &Config) -> anyhow::Result<PublishOutput> {
            self.publish_log
                .lock()
                .expect("publish log mutex")
                .push(self.label.clone());
            Ok(PublishOutput {
                success: true,
                stdout: format!("publish {}", self.label),
                stderr: String::new(),
            })
        }
    }

    /// One configurable mock `Package` for the publish-loop tests. Replaces the
    /// five near-identical hand-written mocks (`FailSpawnPackage`,
    /// `DryRunFailurePackage`, `DryRunUnsupportedPackage`, `RustMockPackage`,
    /// `PublishCascadePackage`): every field value and the `publish` /
    /// `dry_run_publish` outcome is picked per constructor to reproduce each
    /// original mock's exact behavior.
    #[derive(Debug)]
    struct TestPackage {
        name: String,
        version: &'static str,
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        deps: HashSet<String>,
        default_publish_command: &'static str,
        default_dry_run_publish_command: Option<&'static str>,
        publish_behavior: PublishBehavior,
        dry_run_behavior: DryRunBehavior,
        publish_log: Option<Arc<Mutex<Vec<String>>>>,
    }

    /// Outcome produced by [`TestPackage::publish`].
    #[derive(Debug)]
    enum PublishBehavior {
        /// Bail as if the publish process failed to spawn.
        SpawnError,
        /// `Ok(PublishOutput { success, stdout: "publish {name}", stderr: "" })`.
        Succeeds(bool),
        /// The owning mock never has `publish` called; panics if it ever is.
        Unused,
    }

    /// Outcome produced by [`TestPackage::dry_run_publish`].
    #[derive(Debug)]
    enum DryRunBehavior {
        /// Bail as if the dry-run process failed to spawn.
        SpawnError,
        /// `Ok(Some(..))` with `success = false` and captured stdout/stderr.
        NonZeroExit,
        /// `Ok(None)` — the ecosystem does not support a dry-run.
        Unsupported,
        /// `Ok(Some(..))` with `success = true` and stdout `"dry-run ok for {name}"`.
        OkForName,
        /// `Ok(Some(..))` with `success` and stdout `"dry-run {name}"`.
        Succeeds(bool),
    }

    impl TestPackage {
        /// `publish` and `dry_run_publish` both fail to spawn (was `FailSpawnPackage`).
        fn fail_spawn() -> Self {
            Self {
                name: "fail-spawn".to_string(),
                version: "1.0.0",
                path: PathBuf::from("/nonexistent/package.json"),
                relative_path: PathBuf::from("package.json"),
                language: Language::Node,
                deps: HashSet::new(),
                default_publish_command: "echo publish",
                default_dry_run_publish_command: Some("echo publish --dry-run"),
                publish_behavior: PublishBehavior::SpawnError,
                dry_run_behavior: DryRunBehavior::SpawnError,
                publish_log: None,
            }
        }

        /// `dry_run_publish` returns a non-zero-exit output (was `DryRunFailurePackage`).
        fn dry_run_failure() -> Self {
            Self {
                name: "dry-run-failure".to_string(),
                version: "1.0.0",
                path: PathBuf::from("/nonexistent/package.json"),
                relative_path: PathBuf::from("package.json"),
                language: Language::Node,
                deps: HashSet::new(),
                default_publish_command: "echo publish",
                default_dry_run_publish_command: Some("echo publish --dry-run"),
                publish_behavior: PublishBehavior::Unused,
                dry_run_behavior: DryRunBehavior::NonZeroExit,
                publish_log: None,
            }
        }

        /// `dry_run_publish` returns `Ok(None)` (was `DryRunUnsupportedPackage`).
        fn dry_run_unsupported(path: &str) -> Self {
            Self {
                name: "dry-run-unsupported".to_string(),
                version: "1.0.0",
                path: PathBuf::from(path),
                relative_path: PathBuf::from("project.csproj"),
                language: Language::CSharp,
                deps: HashSet::new(),
                default_publish_command: "dotnet nuget push",
                default_dry_run_publish_command: None,
                publish_behavior: PublishBehavior::Unused,
                dry_run_behavior: DryRunBehavior::Unsupported,
                publish_log: None,
            }
        }

        /// Rust leaf whose dry-run succeeds with `"dry-run ok for {name}"`
        /// (was `RustMockPackage`).
        fn rust_mock(name: &str, relative_path: &str, deps: &[&str]) -> Self {
            Self {
                name: name.to_string(),
                version: "0.0.1",
                path: PathBuf::from("Cargo.toml"),
                relative_path: PathBuf::from(relative_path),
                language: Language::Rust,
                deps: deps.iter().map(|d| (*d).to_string()).collect(),
                default_publish_command: "cargo publish",
                default_dry_run_publish_command: Some("cargo publish --dry-run"),
                publish_behavior: PublishBehavior::Unused,
                dry_run_behavior: DryRunBehavior::OkForName,
                publish_log: None,
            }
        }

        fn failing_rust_mock(name: &str, relative_path: &str, deps: &[&str]) -> Self {
            Self {
                name: name.to_string(),
                version: "0.0.1",
                path: PathBuf::from("Cargo.toml"),
                relative_path: PathBuf::from(relative_path),
                language: Language::Rust,
                deps: deps.iter().map(|d| (*d).to_string()).collect(),
                default_publish_command: "cargo publish",
                default_dry_run_publish_command: Some("cargo publish --dry-run"),
                publish_behavior: PublishBehavior::Unused,
                dry_run_behavior: DryRunBehavior::NonZeroExit,
                publish_log: None,
            }
        }

        /// Node package whose publish / dry-run outcome follows `succeeds`
        /// (was `PublishCascadePackage`).
        fn publish_cascade(name: &str, relative_path: &str, deps: &[&str], succeeds: bool) -> Self {
            Self {
                name: name.to_string(),
                version: "1.0.0",
                path: PathBuf::from(relative_path),
                relative_path: PathBuf::from(relative_path),
                language: Language::Node,
                deps: deps.iter().map(|d| (*d).to_string()).collect(),
                default_publish_command: "npm publish",
                default_dry_run_publish_command: Some("npm publish --dry-run"),
                publish_behavior: PublishBehavior::Succeeds(succeeds),
                dry_run_behavior: DryRunBehavior::Succeeds(succeeds),
                publish_log: None,
            }
        }

        fn recording_rust(
            name: &str,
            relative_path: &str,
            dependencies: &[&str],
            publish_log: Arc<Mutex<Vec<String>>>,
        ) -> Self {
            Self {
                name: name.to_string(),
                version: "1.0.0",
                path: PathBuf::from(relative_path),
                relative_path: PathBuf::from(relative_path),
                language: Language::Rust,
                deps: dependencies
                    .iter()
                    .map(|dependency| (*dependency).to_string())
                    .collect(),
                default_publish_command: "cargo publish",
                default_dry_run_publish_command: Some("cargo publish --dry-run"),
                publish_behavior: PublishBehavior::Succeeds(true),
                dry_run_behavior: DryRunBehavior::Succeeds(true),
                publish_log: Some(publish_log),
            }
        }
    }

    #[async_trait::async_trait]
    impl Package for TestPackage {
        fn name(&self) -> Option<&str> {
            Some(&self.name)
        }
        fn version(&self) -> Option<&str> {
            Some(self.version)
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
            self.language
        }
        fn dependencies(&self) -> &HashSet<String> {
            &self.deps
        }
        fn add_dependency(&mut self, dep: &str) {
            self.deps.insert(dep.to_string());
        }
        fn set_changed(&mut self, _changed: bool) {}
        // test mock — name mutation not exercised
        fn set_name(&mut self, _name: String) {}
        fn default_publish_command(&self) -> String {
            self.default_publish_command.to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            self.default_dry_run_publish_command.map(str::to_string)
        }
        async fn publish(&self, _config: &Config) -> anyhow::Result<PublishOutput> {
            if let Some(publish_log) = &self.publish_log {
                publish_log
                    .lock()
                    .expect("publish log mutex")
                    .push(self.name.clone());
            }
            match self.publish_behavior {
                PublishBehavior::SpawnError => {
                    anyhow::bail!("spawn failed: No such file or directory")
                }
                PublishBehavior::Succeeds(succeeds) => Ok(PublishOutput {
                    success: succeeds,
                    stdout: format!("publish {}", self.name),
                    stderr: String::new(),
                }),
                PublishBehavior::Unused => {
                    unreachable!("publish is never called for this mock")
                }
            }
        }
        async fn dry_run_publish(&self, _config: &Config) -> anyhow::Result<Option<PublishOutput>> {
            match self.dry_run_behavior {
                DryRunBehavior::SpawnError => {
                    anyhow::bail!("spawn failed: No such file or directory")
                }
                DryRunBehavior::NonZeroExit => Ok(Some(PublishOutput {
                    success: false,
                    stdout: "dry-run stdout".to_string(),
                    stderr: "dry-run stderr: conflict".to_string(),
                })),
                DryRunBehavior::Unsupported => Ok(None),
                DryRunBehavior::OkForName => Ok(Some(PublishOutput {
                    success: true,
                    stdout: format!("dry-run ok for {}", self.name),
                    stderr: String::new(),
                })),
                DryRunBehavior::Succeeds(succeeds) => Ok(Some(PublishOutput {
                    success: succeeds,
                    stdout: format!("dry-run {}", self.name),
                    stderr: String::new(),
                })),
            }
        }
    }

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
        let pkg = TestPackage::fail_spawn();
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) = execute_publish_loop(&projects, &config, format).await;

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
        let pkg = TestPackage::fail_spawn();
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) = execute_dry_run_publish_loop(&projects, &config, format).await;

        assert_eq!(result_map.len(), expected_result_map_len);
        assert_eq!(failed.len(), 1);
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
        let pkg = TestPackage::dry_run_failure();
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) = execute_dry_run_publish_loop(&projects, &config, format).await;

        assert_eq!(result_map.len(), expected_result_map_len);
        assert_eq!(failed.len(), 1);
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
        let pkg = TestPackage::dry_run_unsupported("/nonexistent/project.csproj");
        let project = Project::Package(Box::new(pkg));
        let projects: Vec<&Project> = vec![&project];
        let config = Config::default();

        let (result_map, failed) = execute_dry_run_publish_loop(&projects, &config, format).await;

        assert_eq!(result_map.len(), expected_result_map_len);
        assert!(failed.is_empty());
    }

    // Direct unit tests for `finish_publish_run` production function.
    // These tests pin the exact bail message format consumed by integration tests/CI.

    #[test]
    fn test_finish_publish_run_empty_failed_projects_stdout() {
        let result_map = BTreeMap::new();
        let failed_projects: Vec<String> = vec![];
        let format = FormatOptions::Stdout;

        let result = finish_publish_run(
            &result_map,
            &failed_projects,
            0,
            format,
            "Dry-run failed for",
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_finish_publish_run_empty_failed_projects_json() {
        let mut result_map = BTreeMap::new();
        // Populate result_map so JSON emission branch executes
        result_map.insert(
            PathBuf::from("packages/test/package.json"),
            PublishResult::new(true, None, "test stdout".to_string(), String::new()),
        );
        let failed_projects: Vec<String> = vec![];
        let format = FormatOptions::Json;

        let result = finish_publish_run(
            &result_map,
            &failed_projects,
            1,
            format,
            "Dry-run failed for",
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_finish_publish_run_two_failed_dry_run() {
        let result_map = BTreeMap::new();
        let failed_projects = vec!["pkg-a".to_string(), "pkg-b".to_string()];
        let format = FormatOptions::Stdout;

        let result = finish_publish_run(
            &result_map,
            &failed_projects,
            2,
            format,
            "Dry-run failed for",
        );

        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(err_msg.contains("Dry-run failed for 2 project(s)"));
        assert!(err_msg.contains("pkg-a"));
        assert!(err_msg.contains("pkg-b"));
    }

    #[test]
    fn test_finish_publish_run_two_failed_publish() {
        let result_map = BTreeMap::new();
        let failed_projects = vec!["pkg-a".to_string(), "pkg-b".to_string()];
        let format = FormatOptions::Stdout;

        let result = finish_publish_run(
            &result_map,
            &failed_projects,
            2,
            format,
            "Failed to publish",
        );

        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(err_msg.contains("Failed to publish 2 project(s)"));
        assert!(err_msg.contains("pkg-a"));
        assert!(err_msg.contains("pkg-b"));
    }

    // Used by leaf packages in the workspace-internal-dep integration tests
    // below. `TestPackage::rust_mock`'s dry-run returns a clean success so the
    // test stays focused on whether the SKIP path is correctly recorded for the
    // parent (the actual cargo invocation we want to avoid).
    fn make_rust_mock(name: &str, relative_path: &str, deps: &[&str]) -> Project {
        Project::Package(Box::new(TestPackage::rust_mock(name, relative_path, deps)))
    }

    fn make_failing_rust_mock(name: &str, relative_path: &str, deps: &[&str]) -> Project {
        Project::Package(Box::new(TestPackage::failing_rust_mock(
            name,
            relative_path,
            deps,
        )))
    }

    fn make_publish_cascade_mock(
        name: &str,
        relative_path: &str,
        deps: &[&str],
        succeeds: bool,
    ) -> Project {
        Project::Package(Box::new(TestPackage::publish_cascade(
            name,
            relative_path,
            deps,
            succeeds,
        )))
    }

    fn make_recording_rust_package(
        name: &str,
        relative_path: &str,
        dependencies: &[&str],
        publish_log: Arc<Mutex<Vec<String>>>,
    ) -> Project {
        Project::Package(Box::new(TestPackage::recording_rust(
            name,
            relative_path,
            dependencies,
            publish_log,
        )))
    }

    fn make_recording_rust_workspace(
        label: &str,
        name: Option<&str>,
        dependencies: &[&str],
        publishable_by_default: bool,
        publish_log: Arc<Mutex<Vec<String>>>,
    ) -> Project {
        Project::Workspace(Box::new(TestWorkspace::recording_rust(
            label,
            name,
            "Cargo.toml",
            dependencies,
            publishable_by_default,
            publish_log,
        )))
    }

    fn recorded_publishes(publish_log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        publish_log.lock().expect("publish log mutex").clone()
    }

    async fn discover_cargo_publish_false_project(root: &std::path::Path) -> RustProjectFinder {
        let cargo_toml = root.join("Cargo.toml");
        std::fs::write(
            &cargo_toml,
            r#"[package]
name = "private-package"
version = "1.0.0"
publish = false
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder
    }

    #[tokio::test]
    async fn test_cargo_publish_false_project_is_excluded_by_default() {
        let temp_dir = tempdir().unwrap();
        let finder = discover_cargo_publish_false_project(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());

        let selected = sort_publishable_projects(discovered, &Config::default(), false).unwrap();
        assert!(selected.is_empty());
    }

    #[rstest]
    #[case("Cargo.toml")]
    #[case("rust")]
    #[tokio::test]
    async fn test_cargo_publish_false_configured_command_forces_inclusion(#[case] key: &str) {
        let temp_dir = tempdir().unwrap();
        let finder = discover_cargo_publish_false_project(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());
        let config = Config {
            publish: BTreeMap::from([(key.to_string(), "custom publish".to_string())]),
            ..Config::default()
        };

        let selected = sort_publishable_projects(discovered, &config, false).unwrap();
        assert_eq!(selected.len(), 1, "override key {key}");
    }

    async fn discover_node_private_project(root: &std::path::Path) -> NodeProjectFinder {
        let package_json = root.join("package.json");
        std::fs::write(
            &package_json,
            r#"{"name":"private-package","version":"1.0.0","private":true}"#,
        )
        .unwrap();

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await
            .unwrap();
        finder
    }

    async fn discover_uv_workspace_only_root(root: &Path) -> PythonProjectFinder {
        let pyproject_toml = root.join("pyproject.toml");
        std::fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await
            .unwrap();
        finder
    }

    #[tokio::test]
    async fn test_uv_workspace_only_root_is_excluded_by_default() {
        let temp_dir = tempdir().unwrap();
        let finder = discover_uv_workspace_only_root(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(matches!(discovered[0], Project::Workspace(_)));
        assert!(!discovered[0].is_publishable_by_default());

        let selected = sort_publishable_projects(discovered, &Config::default(), false).unwrap();
        assert!(selected.is_empty());
    }

    #[rstest]
    #[case("pyproject.toml")]
    #[case("python")]
    #[tokio::test]
    async fn test_uv_workspace_only_root_configured_command_forces_inclusion(#[case] key: &str) {
        let temp_dir = tempdir().unwrap();
        let finder = discover_uv_workspace_only_root(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());
        let config = Config {
            publish: BTreeMap::from([(key.to_string(), "custom publish".to_string())]),
            ..Config::default()
        };

        let selected = sort_publishable_projects(discovered, &config, false).unwrap();
        assert_eq!(selected.len(), 1, "override key {key}");
    }

    #[tokio::test]
    async fn test_node_private_project_is_excluded_by_default() {
        let temp_dir = tempdir().unwrap();
        let finder = discover_node_private_project(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());

        let selected = sort_publishable_projects(discovered, &Config::default(), false).unwrap();
        assert!(selected.is_empty());
    }

    #[rstest]
    #[case("package.json")]
    #[case("node")]
    #[tokio::test]
    async fn test_node_private_configured_command_forces_inclusion(#[case] key: &str) {
        let temp_dir = tempdir().unwrap();
        let finder = discover_node_private_project(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());
        let config = Config {
            publish: BTreeMap::from([(key.to_string(), "custom publish".to_string())]),
            ..Config::default()
        };

        let selected = sort_publishable_projects(discovered, &config, false).unwrap();
        assert_eq!(selected.len(), 1, "override key {key}");
    }

    async fn discover_csharp_is_packable_false_project(
        root: &std::path::Path,
    ) -> CSharpProjectFinder {
        let csproj = root.join("Private.csproj");
        std::fs::write(
            &csproj,
            "<Project><PropertyGroup><IsPackable>false</IsPackable></PropertyGroup></Project>",
        )
        .unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj, &PathBuf::from("Private.csproj"))
            .await
            .unwrap();
        finder
    }

    #[tokio::test]
    async fn test_csharp_is_packable_false_project_is_excluded_by_default() {
        let temp_dir = tempdir().unwrap();
        let finder = discover_csharp_is_packable_false_project(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());

        let selected = sort_publishable_projects(discovered, &Config::default(), false).unwrap();
        assert!(selected.is_empty());
    }

    #[rstest]
    #[case("Private.csproj")]
    #[case("csharp")]
    #[tokio::test]
    async fn test_csharp_is_packable_false_configured_command_forces_inclusion(#[case] key: &str) {
        let temp_dir = tempdir().unwrap();
        let finder = discover_csharp_is_packable_false_project(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());
        let config = Config {
            publish: BTreeMap::from([(key.to_string(), "custom publish".to_string())]),
            ..Config::default()
        };

        let selected = sort_publishable_projects(discovered, &config, false).unwrap();
        assert_eq!(selected.len(), 1, "override key {key}");
    }

    async fn discover_dart_publish_to_none_project(root: &std::path::Path) -> DartProjectFinder {
        let pubspec = root.join("pubspec.yaml");
        std::fs::write(
            &pubspec,
            "name: private_package\nversion: 1.0.0\npublish_to: none\n",
        )
        .unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();
        finder
    }

    #[tokio::test]
    async fn test_dart_publish_to_none_project_is_excluded_by_default() {
        let temp_dir = tempdir().unwrap();
        let finder = discover_dart_publish_to_none_project(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());

        let selected = sort_publishable_projects(discovered, &Config::default(), false).unwrap();
        assert!(selected.is_empty());
    }

    #[rstest]
    #[case("pubspec.yaml")]
    #[case("dart")]
    #[tokio::test]
    async fn test_dart_publish_to_none_configured_command_forces_inclusion(#[case] key: &str) {
        let temp_dir = tempdir().unwrap();
        let finder = discover_dart_publish_to_none_project(temp_dir.path()).await;
        let discovered = finder.projects();
        assert_eq!(discovered.len(), 1);
        assert!(!discovered[0].is_publishable_by_default());
        let config = Config {
            publish: BTreeMap::from([(key.to_string(), "custom publish".to_string())]),
            ..Config::default()
        };

        let selected = sort_publishable_projects(discovered, &config, false).unwrap();
        assert_eq!(selected.len(), 1, "override key {key}");
        match selected[0] {
            Project::Package(package) => {
                assert_eq!(package.get_publish_command(&config), "custom publish");
            }
            Project::Workspace(_) => panic!("expected Dart package"),
        }
    }

    fn gradle_project(
        relative_path: &str,
        has_publish_task: bool,
        has_publish_to_maven_local_task: bool,
    ) -> Project {
        Project::Package(Box::new(GradlePackage::new_with_publish_tasks(
            Some("gradle-project".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from(relative_path),
            PathBuf::from(relative_path),
            has_publish_task,
            has_publish_to_maven_local_task,
        )))
    }

    #[test]
    fn test_gradle_task_availability_selects_normal_vs_dry_run_default() {
        let remote_only = gradle_project("remote/build.gradle.kts", true, false);
        let local_only = gradle_project("local/build.gradle.kts", false, true);
        let projects = vec![&remote_only, &local_only];

        let normal = sort_publishable_projects(projects.clone(), &Config::default(), false)
            .expect("normal Gradle selection should sort");
        let dry_run = sort_publishable_projects(projects, &Config::default(), true)
            .expect("dry-run Gradle selection should sort");

        assert_eq!(
            normal
                .iter()
                .map(|project| project.relative_path())
                .collect::<Vec<_>>(),
            [Path::new("remote/build.gradle.kts")]
        );
        assert_eq!(
            dry_run
                .iter()
                .map(|project| project.relative_path())
                .collect::<Vec<_>>(),
            [Path::new("local/build.gradle.kts")]
        );
    }

    #[rstest]
    #[case(false, "missing/build.gradle.kts")]
    #[case(false, "java")]
    #[case(true, "missing/build.gradle.kts")]
    #[case(true, "java")]
    fn test_gradle_task_availability_configured_overrides_force_inclusion(
        #[case] dry_run: bool,
        #[case] key: &str,
    ) {
        let project = gradle_project("missing/build.gradle.kts", false, false);
        let mut config = Config::default();
        let commands = if dry_run {
            &mut config.publish_dry_run
        } else {
            &mut config.publish
        };
        commands.insert(key.to_string(), "custom publish".to_string());

        let selected = sort_publishable_projects(vec![&project], &config, dry_run)
            .expect("configured Gradle selection should sort");

        assert_eq!(selected.len(), 1, "override key {key}, dry_run={dry_run}");
    }

    #[tokio::test]
    async fn test_virtual_aggregate_is_filtered_and_members_publish_once_in_dependency_order() {
        let publish_log = Arc::new(Mutex::new(Vec::new()));
        let aggregate =
            make_recording_rust_workspace("aggregate", None, &[], false, Arc::clone(&publish_log));
        let parent = make_recording_rust_package(
            "parent",
            "crates/parent/Cargo.toml",
            &["leaf"],
            Arc::clone(&publish_log),
        );
        let leaf = make_recording_rust_package(
            "leaf",
            "crates/leaf/Cargo.toml",
            &[],
            Arc::clone(&publish_log),
        );

        let projects =
            sort_publishable_projects(vec![&aggregate, &parent, &leaf], &Config::default(), false)
                .expect("filtered member graph should be acyclic");

        assert_eq!(
            projects
                .iter()
                .map(|project| project.name())
                .collect::<Vec<_>>(),
            vec![Some("leaf"), Some("parent")]
        );
        let (_, failed) =
            execute_publish_loop(&projects, &Config::default(), FormatOptions::Json).await;
        assert!(failed.is_empty());
        assert_eq!(recorded_publishes(&publish_log), ["leaf", "parent"]);
    }

    #[test]
    fn test_default_nonpublishable_aggregate_is_filtered_before_topological_sort() {
        let publish_log = Arc::new(Mutex::new(Vec::new()));
        // The aggregate and member intentionally form a cycle. Filtering the
        // default-nonpublishable aggregate first removes that irrelevant edge;
        // sorting the unfiltered graph would reject the publish batch.
        let aggregate = make_recording_rust_workspace(
            "aggregate",
            Some("aggregate"),
            &["member"],
            false,
            Arc::clone(&publish_log),
        );
        let member = make_recording_rust_package(
            "member",
            "crates/member/Cargo.toml",
            &["aggregate"],
            publish_log,
        );

        let projects =
            sort_publishable_projects(vec![&aggregate, &member], &Config::default(), false)
                .expect("filtering must happen before cycle detection");

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name(), Some("member"));
    }

    #[tokio::test]
    async fn test_default_publishable_hybrid_root_and_members_publish_once_in_dependency_order() {
        let publish_log = Arc::new(Mutex::new(Vec::new()));
        let root = make_recording_rust_workspace(
            "root",
            Some("root"),
            &[],
            true,
            Arc::clone(&publish_log),
        );
        let parent = make_recording_rust_package(
            "parent",
            "crates/parent/Cargo.toml",
            &["leaf"],
            Arc::clone(&publish_log),
        );
        let leaf = make_recording_rust_package(
            "leaf",
            "crates/leaf/Cargo.toml",
            &[],
            Arc::clone(&publish_log),
        );

        let projects =
            sort_publishable_projects(vec![&parent, &root, &leaf], &Config::default(), false)
                .expect("hybrid root member graph should be acyclic");
        let (_, failed) =
            execute_publish_loop(&projects, &Config::default(), FormatOptions::Json).await;
        assert!(failed.is_empty());

        let publishes = recorded_publishes(&publish_log);
        for name in ["root", "leaf", "parent"] {
            assert_eq!(
                publishes
                    .iter()
                    .filter(|published| *published == name)
                    .count(),
                1,
                "{name} should publish exactly once: {publishes:?}"
            );
        }
        let leaf_index = publishes.iter().position(|name| name == "leaf").unwrap();
        let parent_index = publishes.iter().position(|name| name == "parent").unwrap();
        assert!(leaf_index < parent_index, "dependency order: {publishes:?}");
    }

    async fn assert_configured_virtual_root_publishes(config: Config) {
        let publish_log = Arc::new(Mutex::new(Vec::new()));
        let virtual_root = make_recording_rust_workspace(
            "virtual-root",
            None,
            &[],
            false,
            Arc::clone(&publish_log),
        );

        let projects = sort_publishable_projects(vec![&virtual_root], &config, false)
            .expect("configured virtual root should remain publishable");
        let (_, failed) = execute_publish_loop(&projects, &config, FormatOptions::Json).await;

        assert!(failed.is_empty());
        assert_eq!(recorded_publishes(&publish_log), ["virtual-root"]);
    }

    #[tokio::test]
    async fn test_exact_path_publish_command_supersedes_virtual_root_default() {
        let config = Config {
            publish: BTreeMap::from([("Cargo.toml".to_string(), "custom publish".to_string())]),
            ..Config::default()
        };

        assert_configured_virtual_root_publishes(config).await;
    }

    #[tokio::test]
    async fn test_language_publish_command_supersedes_virtual_root_default() {
        let config = Config {
            publish: BTreeMap::from([("rust".to_string(), "custom publish".to_string())]),
            ..Config::default()
        };

        assert_configured_virtual_root_publishes(config).await;
    }

    #[test]
    fn test_dry_run_command_supersedes_virtual_root_default() {
        for key in ["Cargo.toml", "rust"] {
            let virtual_root = make_recording_rust_workspace(
                "virtual-root",
                None,
                &[],
                false,
                Arc::new(Mutex::new(Vec::new())),
            );
            let config = Config {
                publish_dry_run: BTreeMap::from([(
                    key.to_string(),
                    "custom dry-run publish".to_string(),
                )]),
                ..Config::default()
            };

            let projects = sort_publishable_projects(vec![&virtual_root], &config, true)
                .expect("configured virtual root should remain in dry-run batch");

            assert_eq!(projects.len(), 1, "override key {key}");
        }
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
            execute_publish_loop(&projects, &config, FormatOptions::Json).await;

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

    struct PanicPrompter;

    impl Prompter for PanicPrompter {
        fn multi_select<'a>(
            &self,
            _message: &str,
            _options: Vec<&'a Project>,
            _defaults: Vec<usize>,
        ) -> anyhow::Result<Vec<&'a Project>> {
            panic!("project selection must not be reached")
        }

        fn confirm(&self, _message: &str) -> anyhow::Result<bool> {
            panic!("publish confirmation must not be reached")
        }

        fn text(&self, _message: &str) -> anyhow::Result<String> {
            panic!("text prompt must not be reached")
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_publish_handler_rejects_cycle_before_prompt_dry_run_or_publish_command() {
        let repository = tempdir().expect("create temporary repository");
        init_git_repo(repository.path());
        let alpha_dir = repository.path().join("packages/alpha");
        let beta_dir = repository.path().join("packages/beta");
        let changepacks_dir = repository.path().join(".changepacks");
        std::fs::create_dir_all(&alpha_dir).expect("create alpha directory");
        std::fs::create_dir_all(&beta_dir).expect("create beta directory");
        std::fs::create_dir_all(&changepacks_dir).expect("create config directory");
        std::fs::write(
            alpha_dir.join("package.json"),
            r#"{"name":"alpha","version":"1.0.0","dependencies":{"beta":"^1.0.0"}}"#,
        )
        .expect("write alpha manifest");
        std::fs::write(
            beta_dir.join("package.json"),
            r#"{"name":"beta","version":"1.0.0","dependencies":{"alpha":"^1.0.0"}}"#,
        )
        .expect("write beta manifest");

        let sentinel = repository.path().join("command-reached");
        let sentinel_command = if cfg!(windows) {
            format!("type nul > \"{}\"", sentinel.display())
        } else {
            format!("touch \"{}\"", sentinel.display())
        };
        let config = serde_json::json!({
            "publish": { "node": sentinel_command.clone() },
            "publishDryRun": { "node": sentinel_command },
        });
        std::fs::write(
            changepacks_dir.join("config.json"),
            serde_json::to_vec(&config).expect("serialize config"),
        )
        .expect("write config");
        git_add_and_commit(repository.path(), "cyclic publish fixture");

        let _current_dir_guard = DirGuard::change_to(repository.path());

        for (dry_run, yes) in [(false, false), (true, true), (false, true)] {
            let args = PublishArgs {
                dry_run,
                yes,
                format: FormatOptions::Json,
                remote: false,
                language: vec![],
                project: vec![],
            };

            let error = handle_publish_with_prompter(&args, &PanicPrompter)
                .await
                .expect_err("handler must reject the discovered dependency cycle");
            let message = error.to_string();
            assert!(
                message.starts_with("dependency cycle detected:"),
                "{message}"
            );
            assert!(message.contains("alpha (packages/alpha/package.json)"));
            assert!(message.contains("beta (packages/beta/package.json)"));
            assert!(
                !sentinel.exists(),
                "publish command was executed for dry_run={dry_run}, yes={yes}"
            );
        }
    }

    /// Seeds a committed git repository at `root` holding two independent Node
    /// packages (`packages/alpha`, `packages/beta`) sharing one publish command
    /// that creates a `published` directory in the package's own directory (the
    /// publish runner executes each command in the manifest's parent). Which
    /// markers exist after a publish run is therefore an exact record of which
    /// projects survived the `--project` filter. `mkdir` is used rather than a
    /// redirection so the command needs no shell quoting on either `cmd /C` or
    /// `sh -c`. Returns `(alpha_marker, beta_marker)`.
    fn write_two_package_publish_fixture(root: &Path) -> (PathBuf, PathBuf) {
        init_git_repo(root);
        let changepacks_dir = root.join(".changepacks");
        std::fs::create_dir_all(&changepacks_dir).expect("create config directory");
        for name in ["alpha", "beta"] {
            let package_dir = root.join("packages").join(name);
            std::fs::create_dir_all(&package_dir).expect("create package directory");
            std::fs::write(
                package_dir.join("package.json"),
                format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
            )
            .expect("write manifest");
        }

        let config = serde_json::json!({ "publish": { "node": "mkdir published" } });
        std::fs::write(
            changepacks_dir.join("config.json"),
            serde_json::to_vec(&config).expect("serialize config"),
        )
        .expect("write config");
        git_add_and_commit(root, "project filter fixture");

        (
            root.join("packages/alpha/published"),
            root.join("packages/beta/published"),
        )
    }

    fn publish_args_for_project(project: &str, yes: bool) -> PublishArgs {
        PublishArgs {
            dry_run: false,
            yes,
            format: FormatOptions::Json,
            remote: false,
            language: vec![],
            project: vec![project.to_string()],
        }
    }

    /// Negative side of the `--project` retain predicate: a value naming a path
    /// no discovered project has (a typo, or a project excluded by `ignore`)
    /// must filter the batch down to empty and take the early
    /// `"No projects found"` return — `Ok(())` with nothing published.
    ///
    /// `yes: false` routes any surviving project through the confirmation
    /// prompt, so `PanicPrompter` fails the test if the retain block let
    /// anything through instead of emptying the batch; the markers prove no
    /// publish command ran. The printed message itself is not asserted because
    /// it goes to the process-wide stdout, which these in-process tests cannot
    /// capture; the early return is observed through its side effects instead.
    #[tokio::test]
    #[serial]
    async fn test_publish_project_filter_without_match_publishes_nothing() {
        let repository = tempdir().expect("create temporary repository");
        let (alpha_marker, beta_marker) = write_two_package_publish_fixture(repository.path());

        let _current_dir_guard = DirGuard::change_to(repository.path());

        let args = publish_args_for_project("packages/missing/package.json", false);

        handle_publish_with_prompter(&args, &PanicPrompter)
            .await
            .expect("an unmatched --project value must return Ok(()) with an empty batch");

        assert!(
            !alpha_marker.exists(),
            "alpha must not publish for an unmatched --project value"
        );
        assert!(
            !beta_marker.exists(),
            "beta must not publish for an unmatched --project value"
        );
    }

    /// Positive side of the same retain predicate: the one project whose
    /// relative manifest path matches must survive, and every other discovered
    /// project must be dropped. `yes: true` skips the confirmation prompt (so
    /// `PanicPrompter` is never consulted) and lets the configured publish
    /// command run, leaving exactly one marker behind.
    #[tokio::test]
    #[serial]
    async fn test_publish_project_filter_keeps_only_the_matching_project() {
        let repository = tempdir().expect("create temporary repository");
        let (alpha_marker, beta_marker) = write_two_package_publish_fixture(repository.path());

        let _current_dir_guard = DirGuard::change_to(repository.path());

        let args = publish_args_for_project("packages/alpha/package.json", true);

        handle_publish_with_prompter(&args, &PanicPrompter)
            .await
            .expect("the matched project must publish successfully");

        assert!(
            alpha_marker.exists(),
            "the project matching --project must survive the filter and publish"
        );
        assert!(
            !beta_marker.exists(),
            "a project not named by --project must be filtered out"
        );
    }

    #[tokio::test]
    async fn test_execute_dry_run_loop_skips_dependent_after_failed_dependency() {
        let dependency = make_publish_cascade_mock("pkg-a", "packages/a/package.json", &[], false);
        let dependent =
            make_publish_cascade_mock("pkg-b", "packages/b/package.json", &["pkg-a"], true);
        let projects: Vec<&Project> = vec![&dependency, &dependent];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, FormatOptions::Json).await;

        assert_eq!(failed.len(), 2);
        assert!(failed[0].contains("pkg-a"));
        assert!(failed[1].contains("pkg-b"));
        let dependent_entry = result_map
            .get(std::path::Path::new("packages/b/package.json"))
            .expect("dependent should be recorded as skipped dry-run failure");
        let dependent_serialized = serde_json::to_string(dependent_entry).expect("serialize");
        assert!(dependent_serialized.contains("skipped because dependency failed: pkg-a"));
    }

    #[tokio::test]
    async fn test_execute_dry_run_loop_prioritizes_failed_rust_dependency_over_workspace_skip() {
        let leaf = make_failing_rust_mock("crate-leaf", "crates/leaf/Cargo.toml", &[]);
        let parent = make_rust_mock("crate-parent", "crates/parent/Cargo.toml", &["crate-leaf"]);
        let projects: Vec<&Project> = vec![&leaf, &parent];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, FormatOptions::Json).await;

        assert_eq!(failed.len(), 2);
        assert!(failed[0].contains("crate-leaf"));
        assert!(failed[1].contains("crate-parent"));
        let parent_entry = result_map
            .get(std::path::Path::new("crates/parent/Cargo.toml"))
            .expect("parent should be recorded as skipped dry-run failure");
        let parent_serialized = serde_json::to_string(parent_entry).expect("serialize");
        assert!(parent_serialized.contains("skipped because dependency failed: crate-leaf"));
        assert!(!parent_serialized.contains("dry-run skipped (workspace-internal dep)"));
    }

    #[test]
    fn test_skip_helper_non_rust_returns_false() {
        // CSharp project that happens to declare a dep matching a bumped
        // package: skip must NOT fire because the chicken-and-egg issue is
        // specific to `cargo publish --dry-run`.
        let pkg = TestPackage::dry_run_unsupported("/x/project.csproj");
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
            execute_dry_run_publish_loop(&projects, &config, FormatOptions::Stdout).await;

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
            execute_dry_run_publish_loop(&projects, &config, FormatOptions::Json).await;

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

    #[test]
    fn test_failed_dependency_deterministic_with_multiple_failed_deps() {
        // Project with two failed dependencies: "zebra-dep" and "alpha-dep".
        // failed_dependency must return the lexicographically smallest one ("alpha-dep")
        // to ensure deterministic skip messages across runs.
        let project = make_publish_cascade_mock(
            "pkg-dependent",
            "packages/dependent/package.json",
            &["zebra-dep", "alpha-dep"],
            true,
        );
        let mut failed_names: HashSet<String> = HashSet::new();
        failed_names.insert("zebra-dep".to_string());
        failed_names.insert("alpha-dep".to_string());

        let result = failed_dependency(&project, &failed_names);

        assert_eq!(result, Some("alpha-dep"));
    }

    /// A Rust crate whose dependency name coincides with a *non-Rust* project's
    /// name (both in the same publish batch) must NOT be dry-run-skipped: the
    /// `cargo publish --dry-run` workspace-internal workaround only applies when
    /// the dependency is another *Rust* crate in the same run, so
    /// `rust_batch_names` holds only Rust names. The Node package literally
    /// named `shared` must therefore not shadow the Rust crate's real (external)
    /// `shared` dependency and trigger a spurious skip.
    #[tokio::test]
    async fn test_execute_dry_run_loop_rust_dep_matching_non_rust_name_not_skipped() {
        let rust_crate = make_rust_mock("crate-a", "crates/a/Cargo.toml", &["shared"]);
        let node_named_shared =
            make_publish_cascade_mock("shared", "packages/shared/package.json", &[], true);
        let projects: Vec<&Project> = vec![&rust_crate, &node_named_shared];
        let config = Config::default();

        let (result_map, failed) =
            execute_dry_run_publish_loop(&projects, &config, FormatOptions::Json).await;

        // The Rust crate must run its real dry-run (mock returns
        // "dry-run ok for crate-a"), NOT be recorded as a workspace-internal skip.
        let rust_entry = result_map
            .get(std::path::Path::new("crates/a/Cargo.toml"))
            .expect("rust crate should be recorded with a dry-run result");
        let rust_serialized = serde_json::to_string(rust_entry).expect("serialize");
        assert!(
            rust_serialized.contains("dry-run ok for crate-a"),
            "rust crate should run its dry-run, not be skipped: {rust_serialized}"
        );
        assert!(
            !rust_serialized.contains("dry-run skipped (workspace-internal dep)"),
            "rust crate must not be falsely skipped: {rust_serialized}"
        );
        // The Node package named `shared` is not a Rust crate, so it takes the
        // normal (non-skip) dry-run path and succeeds via the cascade mock.
        assert!(failed.is_empty(), "no project should fail: {failed:?}");
    }
}
