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
use changepacks_core::UpdateType;
use changepacks_utils::next_version;
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
    let current_version = version.as_deref().unwrap_or("0.0.0");
    let new_version = next_version(current_version, update_type)?;
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
    let csproj_raw = read_to_string(path).await?;
    let updated = xml_utils::update_version_in_xml(&csproj_raw, new_version, has_version)?;
    write(path, updated).await?;
    Ok(())
}
