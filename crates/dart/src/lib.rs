//! # changepacks-dart
//!
//! Dart project support for changepacks.
//!
//! Implements project discovery and version management for pubspec.yaml files. Parses YAML
//! using the `yaml_serde` crate while maintaining formatting. Supports both single packages
//! and workspace configurations with pub as the package manager.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::DartProjectFinder;

use std::path::Path;

use anyhow::{Context, Result};
use changepacks_utils::trailing_newline;
use tokio::fs::{read_to_string, write};

/// Update `pubspec.yaml` at `path` to set its `version` field to `new_version`,
/// preserving the file's YAML formatting (via `yamlpatch`/`yamlpath`) and its
/// trailing-newline shape (via `trailing_newline`).
///
/// Shared by `DartPackage::update_version` and `DartWorkspace::update_version`
/// so both paths emit byte-identical output. When `existing_version` is
/// `true`, replaces the existing `version` field; when `false`, adds a new
/// `version` field at the document root (needed for workspace roots that
/// declare no `version`).
///
/// # Errors
/// Returns error if the file cannot be read, is not valid YAML, or the write
/// fails.
pub(crate) async fn write_pubspec_version(
    path: &Path,
    new_version: &str,
    existing_version: bool,
) -> Result<()> {
    let pubspec_yaml_raw = read_to_string(path).await?;
    let patch = if existing_version {
        yamlpatch::Patch {
            operation: yamlpatch::Op::Replace(yaml_serde::Value::String(new_version.to_string())),
            route: yamlpath::route!("version"),
        }
    } else {
        yamlpatch::Patch {
            operation: yamlpatch::Op::Add {
                key: "version".to_string(),
                value: yaml_serde::Value::String(new_version.to_string()),
            },
            route: yamlpath::route!(),
        }
    };
    write(
        path,
        format!(
            "{}{}",
            yamlpatch::apply_yaml_patches(
                &yamlpath::Document::new(&pubspec_yaml_raw).context("Failed to parse YAML")?,
                &[patch],
            )?
            .source()
            .trim_end(),
            trailing_newline(&pubspec_yaml_raw)
        ),
    )
    .await?;
    Ok(())
}
