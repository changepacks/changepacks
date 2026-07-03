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
pub use version_updater::{
    update_gradle_version_at, update_version_in_groovy, update_version_in_kts,
};

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
