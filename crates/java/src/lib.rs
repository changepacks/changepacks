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
pub use version_updater::{update_version_in_groovy, update_version_in_kts};
