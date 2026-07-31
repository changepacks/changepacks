//! # changepacks-java
//!
//! Java/Gradle project support for changepacks.
//!
//! Implements project discovery and version management for Gradle build files (build.gradle,
//! build.gradle.kts). Handles both Groovy and Kotlin DSL syntax for version declarations.
//! Requires the Gradle wrapper (gradlew) for dynamic version detection.

pub mod finder;
mod gradle_dependency_lexer;
mod gradle_metadata;
pub mod package;
mod properties_version;
#[cfg(test)]
pub(crate) mod test_support;
mod version_lexer;
pub mod version_updater;
pub mod workspace;

pub use finder::GradleProjectFinder;
pub use version_updater::write_gradle_version;

use anyhow::{Context, Result};
use changepacks_core::{Config, UpdateType};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};

// Per-OS Gradle wrapper commands. Windows uses `gradlew.bat` and backslash;
// every other target uses the POSIX `./gradlew` shell script. These consts
// are shared by `GradlePackage` and `GradleWorkspace` so a single edit
// updates both trait impls without drift.
//
// Gradle's built-in `--dry-run` only previews the task graph, so we run the
// full publish pipeline against an isolated temporary Maven local repository
// via `publishToMavenLocal` instead for dry-runs.
#[cfg(windows)]
pub(crate) const PUBLISH_COMMAND: &str = ".\\gradlew.bat publish";
#[cfg(not(windows))]
pub(crate) const PUBLISH_COMMAND: &str = "./gradlew publish";

#[cfg(windows)]
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = ".\\gradlew.bat publishToMavenLocal";
#[cfg(not(windows))]
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = "./gradlew publishToMavenLocal";

/// Expand to the three inherent constructors shared verbatim by
/// `GradlePackage` and `GradleWorkspace`.
///
/// Both structs carry exactly the same nine fields and built the same
/// three-step constructor chain (`new` -> `new_with_publish_tasks` ->
/// `new_with_project_path_and_publish_tasks`) from byte-identical bodies;
/// only the order the fields happened to be declared in differed. The
/// final struct literal therefore uses field-init shorthand, which is
/// order-insensitive, so one expansion serves both declaration orders.
///
/// Invoked from inside an `impl GradlePackage` or `impl GradleWorkspace`
/// block. Fully-qualified `::std::option::Option`,
/// `::std::string::String`, `::std::path::PathBuf` and
/// `::std::collections::HashSet` keep the macro hygienic — callers do not
/// need those types in scope at the invocation site.
///
/// Consumer requirement: the struct must have `name`, `version`, `path`,
/// `relative_path`, `project_path`, `is_changed`, `dependencies`,
/// `has_publish_task` and `has_publish_to_maven_local_task` fields.
/// `GradlePackage` and `GradleWorkspace` are the only two intended callers.
macro_rules! impl_gradle_constructors {
    () => {
        #[must_use]
        pub fn new(
            name: ::std::option::Option<::std::string::String>,
            version: ::std::option::Option<::std::string::String>,
            path: ::std::path::PathBuf,
            relative_path: ::std::path::PathBuf,
        ) -> Self {
            Self::new_with_publish_tasks(name, version, path, relative_path, true, true)
        }

        #[must_use]
        pub fn new_with_publish_tasks(
            name: ::std::option::Option<::std::string::String>,
            version: ::std::option::Option<::std::string::String>,
            path: ::std::path::PathBuf,
            relative_path: ::std::path::PathBuf,
            has_publish_task: bool,
            has_publish_to_maven_local_task: bool,
        ) -> Self {
            Self::new_with_project_path_and_publish_tasks(
                name,
                version,
                path,
                relative_path,
                ::std::option::Option::None,
                has_publish_task,
                has_publish_to_maven_local_task,
            )
        }

        #[must_use]
        pub(crate) fn new_with_project_path_and_publish_tasks(
            name: ::std::option::Option<::std::string::String>,
            version: ::std::option::Option<::std::string::String>,
            path: ::std::path::PathBuf,
            relative_path: ::std::path::PathBuf,
            project_path: ::std::option::Option<::std::string::String>,
            has_publish_task: bool,
            has_publish_to_maven_local_task: bool,
        ) -> Self {
            Self {
                name,
                version,
                path,
                relative_path,
                project_path,
                is_changed: false,
                dependencies: ::std::collections::HashSet::new(),
                has_publish_task,
                has_publish_to_maven_local_task,
            }
        }
    };
}

pub(crate) use impl_gradle_constructors;

/// Declare a Gradle project struct plus its shared inherent constructors.
///
/// `GradlePackage` and `GradleWorkspace` carry exactly the same nine fields
/// — previously declared in two different orders — and each followed the
/// declaration with an inherent impl containing
/// [`impl_gradle_constructors!`], which already hard-codes those field
/// names. Canonicalizing the layout here keeps the two in lockstep; the
/// expansion's struct literal uses field-init shorthand, so the former
/// order difference was cosmetic. Java cannot use
/// `changepacks_core::declare_discovered_project!`: it carries
/// `project_path` plus the two publish-task flags and has no
/// `publishable_by_default` field. Outer attributes (including doc
/// comments) pass through; other inherent methods and every trait impl stay
/// in separate blocks beside the invocation.
macro_rules! declare_gradle_project {
    ($(#[$meta:meta])* pub struct $name:ident) => {
        $(#[$meta])*
        #[derive(::std::fmt::Debug)]
        pub struct $name {
            name: ::std::option::Option<::std::string::String>,
            version: ::std::option::Option<::std::string::String>,
            path: ::std::path::PathBuf,
            relative_path: ::std::path::PathBuf,
            project_path: ::std::option::Option<::std::string::String>,
            is_changed: ::std::primitive::bool,
            dependencies: ::std::collections::HashSet<::std::string::String>,
            has_publish_task: ::std::primitive::bool,
            has_publish_to_maven_local_task: ::std::primitive::bool,
        }

        impl $name {
            crate::impl_gradle_constructors!();
        }
    };
}

pub(crate) use declare_gradle_project;

/// Compute the next semver version and write it into the Gradle build file.
///
/// `GradlePackage::update_version` and `GradleWorkspace::update_version` had
/// byte-identical bodies apart from the [`GradleVersionScope`] they select
/// (`ScriptOnly` for a package, `ScriptAndAllProjects` for a workspace root),
/// so the shared body lives here and each trait method is a single delegating
/// call that supplies its own scope.
///
/// This is a plain free function rather than a `macro_rules!`: `#[async_trait]`
/// rewrites the `impl` block before macro bodies expand, so a macro invocation
/// inside the impl would emit an `async fn` that no longer matches the
/// desugared trait signature (E0195) — see the matching note in `package.rs`.
///
/// [`GradleVersionScope`]: version_updater::GradleVersionScope
///
/// # Errors
/// Returns an error when the next version cannot be computed, or when the
/// Gradle build file (or its sibling `gradle.properties`) cannot be rewritten.
/// A failed write leaves `version` untouched.
pub(crate) async fn bump_gradle_version(
    version: &mut Option<String>,
    path: &Path,
    update_type: UpdateType,
    scope: version_updater::GradleVersionScope,
) -> Result<()> {
    changepacks_utils::bump_version_with(version, path, update_type, async |new| {
        write_gradle_version(path, new, scope).await
    })
    .await
}

fn finish_isolated_gradle_dry_run(
    publish_result: Result<changepacks_core::publish::PublishOutput>,
    cleanup_result: Result<()>,
) -> Result<changepacks_core::publish::PublishOutput> {
    match (publish_result, cleanup_result) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(publish_error), Ok(())) => Err(publish_error),
        (Ok(output), Err(cleanup_error)) => {
            let outcome = if output.success {
                "succeeded"
            } else {
                "reported failure"
            };
            Err(anyhow::anyhow!(
                "Gradle dry run {outcome}; stdout: {}; stderr: {}; failed to remove isolated temporary Maven local repository: {cleanup_error:#}",
                output.stdout,
                output.stderr,
            ))
        }
        (Err(publish_error), Err(cleanup_error)) => Err(anyhow::anyhow!(
            "Gradle dry run failed: {publish_error:#}; failed to remove isolated temporary Maven local repository: {cleanup_error:#}"
        )),
    }
}

async fn run_built_in_gradle_dry_run_with<Run, RunFuture>(
    run_gradle: Run,
) -> Result<changepacks_core::publish::PublishOutput>
where
    Run: FnOnce(PathBuf) -> RunFuture,
    RunFuture: Future<Output = Result<changepacks_core::publish::PublishOutput>>,
{
    let maven_local = tempfile::Builder::new()
        .prefix("changepacks-maven-local-")
        .tempdir()
        .context("Failed to create isolated temporary Maven local repository")?;
    let repository = maven_local.path().to_path_buf();
    let publish_result = run_gradle(repository).await;
    let cleanup_result = maven_local
        .close()
        .context("Failed to remove isolated temporary Maven local repository");

    finish_isolated_gradle_dry_run(publish_result, cleanup_result)
}

pub(crate) async fn run_publish_for_path(
    path: &Path,
    relative_path: &Path,
    project_path: Option<&str>,
    config: &Config,
    missing_dir_message: &'static str,
) -> Result<changepacks_core::publish::PublishOutput> {
    if let Some(command) = resolve_publish_override(&config.publish, relative_path) {
        return changepacks_core::publish::run_publish_flow(
            &command,
            path,
            &[],
            missing_dir_message,
        )
        .await;
    }

    finder::run_gradle_publish(
        path,
        relative_path,
        project_path,
        "publish",
        &[],
        missing_dir_message,
    )
    .await
}

pub(crate) async fn run_dry_run_publish_for_path(
    path: &Path,
    relative_path: &Path,
    project_path: Option<&str>,
    config: &Config,
    missing_dir_message: &'static str,
) -> Result<Option<changepacks_core::publish::PublishOutput>> {
    if let Some(command) = resolve_publish_override(&config.publish_dry_run, relative_path) {
        return changepacks_core::publish::run_dry_run_publish_flow(
            Some(&command),
            path,
            &[],
            missing_dir_message,
        )
        .await;
    }

    run_built_in_gradle_dry_run_with(|maven_local| async move {
        let mut repository_argument = OsString::from("-Dmaven.repo.local=");
        repository_argument.push(maven_local);
        finder::run_gradle_publish(
            path,
            relative_path,
            project_path,
            "publishToMavenLocal",
            &[repository_argument],
            missing_dir_message,
        )
        .await
    })
    .await
    .map(Some)
}

fn resolve_publish_override(
    commands: &BTreeMap<String, String>,
    relative_path: &Path,
) -> Option<String> {
    changepacks_core::publish::lookup_by_path_or_language(
        commands,
        relative_path,
        changepacks_core::Language::Java,
    )
    .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_isolation_asserting_wrapper(root: &Path, exit_code: i32) {
        #[cfg(windows)]
        fs::write(
            root.join("gradlew.bat"),
            format!(
                "@echo off\n\
                 if not \"%~1\"==\":libs:core:publishToMavenLocal\" (\n\
                   echo unexpected task: %~1 1>&2\n\
                   exit /b 41\n\
                 )\n\
                 if not \"%~3\"==\"\" (\n\
                   echo unexpected additional argument: %~3 1>&2\n\
                   exit /b 44\n\
                 )\n\
                 set \"repo_argument=%~2\"\n\
                 if not \"%repo_argument:~0,19%\"==\"-Dmaven.repo.local=\" (\n\
                   echo missing isolated Maven repository argument 1>&2\n\
                   exit /b 42\n\
                 )\n\
                 set \"repo=%repo_argument:~19%\"\n\
                 if not exist \"%repo%\" (\n\
                   echo isolated Maven repository did not exist during execution 1>&2\n\
                   exit /b 43\n\
                 )\n\
                 echo isolated_repo=%repo%\n\
                 exit /b {exit_code}\n"
            ),
        )
        .unwrap();

        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;

            let wrapper = root.join("gradlew");
            fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\n\
                     if [ \"$#\" -ne 2 ]; then\n\
                       printf 'expected exactly two arguments, received %s\\n' \"$#\" >&2\n\
                       exit 44\n\
                     fi\n\
                     if [ \"$1\" != ':libs:core:publishToMavenLocal' ]; then\n\
                       printf 'unexpected task: %s\\n' \"$1\" >&2\n\
                       exit 41\n\
                     fi\n\
                     repo_argument=$2\n\
                     case $repo_argument in\n\
                       -Dmaven.repo.local=*) repo=${{repo_argument#-Dmaven.repo.local=}} ;;\n\
                       *) printf 'missing isolated Maven repository argument\\n' >&2; exit 42 ;;\n\
                     esac\n\
                     if [ ! -d \"$repo\" ]; then\n\
                       printf 'isolated Maven repository did not exist during execution\\n' >&2\n\
                       exit 43\n\
                     fi\n\
                     printf 'isolated_repo=%s\\n' \"$repo\"\n\
                     exit {exit_code}\n"
                ),
            )
            .unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn nested_gradle_manifest(temp_dir: &TempDir) -> (PathBuf, PathBuf) {
        let root = temp_dir.path().join("repo with spaces");
        let project_dir = root.join("libs").join("core");
        fs::create_dir_all(&project_dir).unwrap();
        let manifest = project_dir.join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        (root, manifest)
    }

    fn isolated_repository_from(output: &changepacks_core::publish::PublishOutput) -> PathBuf {
        output
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("isolated_repo="))
            .map(PathBuf::from)
            .expect("fake Gradle wrapper did not report its isolated Maven repository")
    }

    fn captured_publish_output(success: bool) -> changepacks_core::publish::PublishOutput {
        changepacks_core::publish::PublishOutput {
            success,
            stdout: "captured Gradle stdout".to_string(),
            stderr: "captured Gradle stderr".to_string(),
        }
    }

    fn create_no_args_override(project_dir: &Path) -> String {
        #[cfg(windows)]
        {
            fs::write(
                project_dir.join("override-check.bat"),
                "@echo off\n\
                 if not \"%~1\"==\"\" (\n\
                   echo unexpected argument: %~1 1>&2\n\
                   exit /b 51\n\
                 )\n\
                 echo override-without-injected-args\n",
            )
            .unwrap();
            "call override-check.bat".to_string()
        }

        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;

            let script = project_dir.join("override-check.sh");
            fs::write(
                &script,
                "#!/bin/sh\n\
                 if [ \"$#\" -ne 0 ]; then\n\
                   printf 'unexpected argument: %s\\n' \"$1\" >&2\n\
                   exit 51\n\
                 fi\n\
                 printf 'override-without-injected-args\\n'\n",
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            "./override-check.sh".to_string()
        }
    }

    #[test]
    fn test_resolve_publish_override_prefers_path_then_language() {
        let relative_path = Path::new("libs/core/build.gradle.kts");
        let mut commands = BTreeMap::new();
        commands.insert("java".to_string(), "language-command".to_string());
        commands.insert(
            relative_path.to_string_lossy().into_owned(),
            "path-command".to_string(),
        );

        assert_eq!(
            resolve_publish_override(&commands, relative_path).as_deref(),
            Some("path-command")
        );
        commands.remove(relative_path.to_string_lossy().as_ref());
        assert_eq!(
            resolve_publish_override(&commands, relative_path).as_deref(),
            Some("language-command")
        );
        commands.clear();
        assert_eq!(resolve_publish_override(&commands, relative_path), None);
    }

    #[test]
    fn finish_isolated_dry_run_returns_output_when_cleanup_succeeds() {
        let result =
            finish_isolated_gradle_dry_run(Ok(captured_publish_output(true)), Ok(())).unwrap();

        assert!(result.success);
        assert_eq!(result.stdout, "captured Gradle stdout");
        assert_eq!(result.stderr, "captured Gradle stderr");
    }

    #[test]
    fn finish_isolated_dry_run_returns_execution_error_when_cleanup_succeeds() {
        let error = finish_isolated_gradle_dry_run(
            Err(anyhow::anyhow!("wrapper execution failed")),
            Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "wrapper execution failed");
    }

    #[test]
    fn finish_isolated_dry_run_reports_cleanup_error_after_successful_output() {
        let error = finish_isolated_gradle_dry_run(
            Ok(captured_publish_output(true)),
            Err(anyhow::anyhow!("injected cleanup failure")),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("Gradle dry run succeeded"), "{error}");
        assert!(error.contains("captured Gradle stdout"), "{error}");
        assert!(error.contains("captured Gradle stderr"), "{error}");
        assert!(error.contains("injected cleanup failure"), "{error}");
    }

    #[test]
    fn finish_isolated_dry_run_retains_nonzero_output_when_cleanup_fails() {
        let error = finish_isolated_gradle_dry_run(
            Ok(captured_publish_output(false)),
            Err(anyhow::anyhow!("injected cleanup failure")),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("Gradle dry run reported failure"), "{error}");
        assert!(error.contains("captured Gradle stdout"), "{error}");
        assert!(error.contains("captured Gradle stderr"), "{error}");
        assert!(error.contains("injected cleanup failure"), "{error}");
    }

    #[test]
    fn finish_isolated_dry_run_reports_execution_and_cleanup_errors() {
        let error = finish_isolated_gradle_dry_run(
            Err(anyhow::anyhow!("wrapper execution failed")),
            Err(anyhow::anyhow!("injected cleanup failure")),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("wrapper execution failed"), "{error}");
        assert!(error.contains("injected cleanup failure"), "{error}");
    }

    #[tokio::test]
    async fn built_in_dry_run_cleans_isolated_repository_when_wrapper_is_missing() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        let mut observed_repository = None;

        let result = run_built_in_gradle_dry_run_with(|maven_local| {
            observed_repository = Some(maven_local.clone());
            async move {
                let mut repository_argument = OsString::from("-Dmaven.repo.local=");
                repository_argument.push(maven_local);
                finder::run_gradle_publish(
                    &manifest,
                    Path::new("build.gradle.kts"),
                    Some(":"),
                    "publishToMavenLocal",
                    &[repository_argument],
                    "Package directory not found",
                )
                .await
            }
        })
        .await;

        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("Gradle wrapper (gradlew) not found"),
            "{error}"
        );
        let observed_repository = observed_repository.unwrap();
        assert!(
            !observed_repository.exists(),
            "temporary Maven repository survived wrapper lookup failure: {}",
            observed_repository.display()
        );
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn built_in_dry_run_uses_exact_subproject_task_and_removes_isolated_repository() {
        let temp_dir = TempDir::new().unwrap();
        let (root, manifest) = nested_gradle_manifest(&temp_dir);
        create_isolation_asserting_wrapper(&root, 0);
        let relative_path = Path::new("libs/core/build.gradle.kts");

        let output = run_dry_run_publish_for_path(
            &manifest,
            relative_path,
            Some(":libs:core"),
            &Config::default(),
            "Package directory not found",
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        let isolated_repository = isolated_repository_from(&output);
        assert!(
            isolated_repository.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("changepacks-maven-local-")),
            "Gradle did not receive a changepacks-owned temporary repository: {}",
            isolated_repository.display()
        );
        assert!(
            !isolated_repository.exists(),
            "temporary Maven repository was not cleaned up: {}",
            isolated_repository.display()
        );
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn built_in_dry_run_removes_isolated_repository_after_gradle_failure() {
        let temp_dir = TempDir::new().unwrap();
        let (root, manifest) = nested_gradle_manifest(&temp_dir);
        create_isolation_asserting_wrapper(&root, 29);
        let relative_path = Path::new("libs/core/build.gradle.kts");

        let output = run_dry_run_publish_for_path(
            &manifest,
            relative_path,
            Some(":libs:core"),
            &Config::default(),
            "Package directory not found",
        )
        .await
        .unwrap()
        .unwrap();

        assert!(!output.success);
        let isolated_repository = isolated_repository_from(&output);
        assert!(
            !isolated_repository.exists(),
            "temporary Maven repository was not cleaned up after failure: {}",
            isolated_repository.display()
        );
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn configured_dry_run_override_receives_no_injected_argument() {
        let temp_dir = TempDir::new().unwrap();
        let (_root, manifest) = nested_gradle_manifest(&temp_dir);
        let project_dir = manifest.parent().unwrap();
        let mut publish_dry_run = BTreeMap::new();
        publish_dry_run.insert("java".to_string(), create_no_args_override(project_dir));
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        let output = run_dry_run_publish_for_path(
            &manifest,
            Path::new("libs/core/build.gradle.kts"),
            Some(":ignored:for:override"),
            &config,
            "Package directory not found",
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert_eq!(output.stdout.trim(), "override-without-injected-args");
        temp_dir.close().unwrap();
    }
}
