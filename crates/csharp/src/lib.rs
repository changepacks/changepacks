//! # changepacks-csharp
//!
//! C#/.NET project support for changepacks.
//!
//! Implements project discovery and version management for .csproj XML files. Uses quick-xml
//! for parsing with format preservation. Supports `MSBuild` project files with version elements
//! and handles both single projects and multi-project solutions.

mod dry_run;
pub mod finder;
pub mod package;
pub mod workspace;
mod xml_utils;

use std::path::Path;

use anyhow::{Context, Result};
use changepacks_core::UpdateType;
use changepacks_utils::next_version_or_default;
use tokio::fs::{read_to_string, write};

pub use finder::CSharpProjectFinder;

/// Default publish command for C#/.NET projects. Shared by `CSharpPackage`
/// and `CSharpWorkspace` so a single edit here updates both trait impls.
///
/// `dotnet nuget push` has no native `--dry-run` mode, so
/// `default_dry_run_publish_command` returns `None` in both impls and the
/// actual dry-run flow lives in the RAII-managed `dry_run_publish`
/// override (`crate::dry_run::resolve_and_run_dry_run`).
pub(crate) const PUBLISH_COMMAND: &str = "dotnet pack -c Release && dotnet nuget push";

/// Shared body for `CSharpPackage::update_version` and
/// `CSharpWorkspace::update_version`.
///
/// Consolidates the "read current version, compute next, write csproj,
/// stash new version on `self`" 5-line sequence — previously duplicated
/// byte-for-byte between `CSharpPackage` and `CSharpWorkspace` — into ONE
/// source of truth. Both trait impls now delegate here so a future
/// rewording of the "reserve `0.0.0` when unversioned" fallback or the
/// `has_version` derivation lands in exactly one place.
///
/// A shared helper (rather than a `macro_rules!` mirroring
/// `impl_node_publish_wiring!()`) is required because `#[async_trait]`
/// runs BEFORE declarative-macro expansion — see the twin helper in
/// `crates/dart/src/lib.rs` for the full E0195 rationale.
///
/// # Errors
/// Returns error if the version update or csproj write fails.
pub(crate) async fn update_version_from_fields(
    version: &mut Option<String>,
    path: &Path,
    update_type: UpdateType,
) -> Result<()> {
    // Two-line "reserve `0.0.0` when unversioned" prelude consolidated
    // into `changepacks_utils::next_version_or_default` so the fallback
    // policy lives in ONE place across every language crate. See that
    // helper's doc for the Java/Rust carve-outs.
    let new_version = next_version_or_default(version.as_deref(), update_type)?;
    write_csproj_version(path, &new_version, version.is_some()).await?;
    *version = Some(new_version);
    Ok(())
}

/// Update the `<Version>` element of the `.csproj` XML at `path` to
/// `new_version`, delegating to [`xml_utils::update_version_in_xml`] to
/// preserve the file's original formatting (indentation, comments, sibling
/// elements). When `has_version` is `false`, a new `<Version>` element is
/// added under the first `<PropertyGroup>` (see `update_version_in_xml`).
///
/// Shared by `CSharpPackage::update_version` and `CSharpWorkspace::update_version`
/// so both paths emit byte-identical output — matching the Node/Python/Dart
/// convention documented in `crates/AGENTS.md`.
///
/// # Errors
/// Returns error if the file cannot be read, the XML cannot be parsed, or
/// the write fails.
pub(crate) async fn write_csproj_version(
    path: &Path,
    new_version: &str,
    has_version: bool,
) -> Result<()> {
    let csproj_raw = read_to_string(path)
        .await
        .with_context(|| format!("Failed to read C# project {}", path.display()))?;
    let updated = xml_utils::update_version_in_xml(&csproj_raw, new_version, has_version)?;
    if updated != csproj_raw {
        write(path, updated)
            .await
            .with_context(|| format!("Failed to write C# project {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_csproj_version_skips_unchanged_readonly_file() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("NoPropertyGroup.csproj");
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n</Project>\n";
        tokio::fs::write(&csproj_path, content).await.unwrap();

        let mut permissions = std::fs::metadata(&csproj_path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&csproj_path, permissions).unwrap();

        let result = write_csproj_version(&csproj_path, "1.2.3", false).await;

        let mut permissions = std::fs::metadata(&csproj_path).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&csproj_path, permissions).unwrap();

        result.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&csproj_path).await.unwrap(),
            content
        );
        temp_dir.close().unwrap();
    }
}
