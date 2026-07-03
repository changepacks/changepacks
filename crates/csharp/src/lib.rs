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

use anyhow::Result;
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
    let csproj_raw = read_to_string(path).await?;
    let updated = xml_utils::update_version_in_xml(&csproj_raw, new_version, has_version)?;
    write(path, updated).await?;
    Ok(())
}
