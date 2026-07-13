//! # changepacks-java
//!
//! Java/Gradle project support for changepacks.
//!
//! Implements project discovery and version management for Gradle build files (build.gradle,
//! build.gradle.kts). Handles both Groovy and Kotlin DSL syntax for version declarations.
//! Requires the Gradle wrapper (gradlew) for dynamic version detection.

pub mod finder;
pub mod package;
pub mod version_updater;
pub mod workspace;

pub use finder::GradleProjectFinder;
pub use version_updater::{update_version_in_groovy, update_version_in_kts, write_gradle_version};

#[cfg(not(tarpaulin_include))]
use anyhow::Result;
#[cfg(not(tarpaulin_include))]
use changepacks_core::Config;
#[cfg(not(tarpaulin_include))]
use std::path::Path;

// Per-OS Gradle wrapper commands. Windows uses `gradlew.bat` and backslash;
// every other target uses the POSIX `./gradlew` shell script. These consts
// are shared by `GradlePackage` and `GradleWorkspace` so a single edit
// updates both trait impls without drift.
//
// Gradle's built-in `--dry-run` only previews the task graph, so we run the
// full publish pipeline against the local Maven cache
// (`publishToMavenLocal` → `~/.m2/repository`) instead for dry-runs.
#[cfg(windows)]
pub(crate) const PUBLISH_COMMAND: &str = ".\\gradlew.bat publish";
#[cfg(not(windows))]
pub(crate) const PUBLISH_COMMAND: &str = "./gradlew publish";

#[cfg(windows)]
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = ".\\gradlew.bat publishToMavenLocal";
#[cfg(not(windows))]
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = "./gradlew publishToMavenLocal";

#[cfg(not(tarpaulin_include))]
pub(crate) async fn run_publish_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_message: &'static str,
) -> Result<changepacks_core::publish::PublishOutput> {
    if let Some(command) = changepacks_core::publish::lookup_by_path_or_language(
        &config.publish,
        relative_path,
        changepacks_core::Language::Java,
    ) {
        return changepacks_core::publish::run_publish_flow(
            &command,
            path,
            &[],
            missing_dir_message,
        )
        .await;
    }

    finder::run_gradle_publish(path, relative_path, "publish", missing_dir_message).await
}

#[cfg(not(tarpaulin_include))]
pub(crate) async fn run_dry_run_publish_for_path(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_message: &'static str,
) -> Result<Option<changepacks_core::publish::PublishOutput>> {
    if let Some(command) = changepacks_core::publish::lookup_by_path_or_language(
        &config.publish_dry_run,
        relative_path,
        changepacks_core::Language::Java,
    ) {
        return changepacks_core::publish::run_dry_run_publish_flow(
            Some(&command),
            path,
            &[],
            missing_dir_message,
        )
        .await;
    }

    finder::run_gradle_publish(
        path,
        relative_path,
        "publishToMavenLocal",
        missing_dir_message,
    )
    .await
    .map(Some)
}
