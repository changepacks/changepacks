//! # changepacks-dart
//!
//! Dart project support for changepacks.
//!
//! Implements project discovery and version management for pubspec.yaml files. Parses YAML
//! with `yaml_serde` and applies format-preserving edits via `yamlpatch`/`yamlpath`. Supports
//! both single packages and workspace configurations with pub as the package manager.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::DartProjectFinder;

/// Default publish command for Dart projects. Shared by `DartPackage`
/// and `DartWorkspace` so a single edit here updates both trait impls.
pub(crate) const PUBLISH_COMMAND: &str = "dart pub publish";

/// Default dry-run publish command for Dart projects.
/// `dart pub publish --dry-run` performs the full pre-flight publish
/// validation (analysis, deps check, LICENSE/CHANGELOG detection) without
/// uploading. Users can override via `publishDryRun` in config.
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = "dart pub publish --dry-run";

use std::path::Path;

use anyhow::{Context, Result};
use changepacks_utils::finalize_content;
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
    let pubspec_yaml_raw = read_to_string(path)
        .await
        .with_context(|| format!("Failed to read pubspec.yaml {}", path.display()))?;
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
    let patched = yamlpatch::apply_yaml_patches(
        &yamlpath::Document::new(&pubspec_yaml_raw)
            .with_context(|| format!("Failed to parse pubspec.yaml {}", path.display()))?,
        &[patch],
    )
    .with_context(|| format!("Failed to update pubspec.yaml {}", path.display()))?;
    write(
        path,
        finalize_content(patched.source().to_string(), &pubspec_yaml_raw),
    )
    .await
    .with_context(|| format!("Failed to write pubspec.yaml {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_pubspec_version_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_yaml = temp_dir.path().join("pubspec.yaml");
        fs::write(&pubspec_yaml, "name: test\nversion: 1.0.0\n").unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        let mut permissions = fs::metadata(&pubspec_yaml).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&pubspec_yaml, permissions).unwrap();

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result = write_pubspec_version(&pubspec_yaml, "2.0.0", true).await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        let mut permissions = fs::metadata(&pubspec_yaml).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&pubspec_yaml, permissions).unwrap();

        let err = result.expect_err("write to a readonly pubspec.yaml must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pubspec_yaml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }
}
