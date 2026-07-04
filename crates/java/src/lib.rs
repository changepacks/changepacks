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

use std::path::Path;

use anyhow::Result;
use changepacks_core::UpdateType;

pub use finder::GradleProjectFinder;
pub use version_updater::{
    update_gradle_version_at, update_version_in_groovy, update_version_in_kts,
};

/// Shared body for `GradlePackage::update_version` and
/// `GradleWorkspace::update_version`.
///
/// Consolidates the "read current version, delegate to
/// `update_gradle_version_at`, stash new version on `self`" 4-line
/// sequence — previously duplicated byte-for-byte between `GradlePackage`
/// and `GradleWorkspace` — into ONE source of truth. Both trait impls now
/// delegate here so a future rewording of the "reserve `0.0.0` when
/// unversioned" fallback lands in exactly one place.
///
/// A shared helper (rather than a `macro_rules!` mirroring
/// `impl_node_publish_wiring!()`) is required because `#[async_trait]`
/// runs BEFORE declarative-macro expansion — see the twin helper in
/// `crates/dart/src/lib.rs` for the full E0195 rationale.
///
/// # Errors
/// Returns error if the gradle version update fails.
pub(crate) async fn update_version_from_fields(
    version: &mut Option<String>,
    path: &Path,
    update_type: UpdateType,
) -> Result<()> {
    let current_version = version.as_deref().unwrap_or("0.0.0");
    let new_version = update_gradle_version_at(path, current_version, update_type).await?;
    *version = Some(new_version);
    Ok(())
}

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
