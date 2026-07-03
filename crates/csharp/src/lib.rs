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

pub use finder::CSharpProjectFinder;

/// Default publish command for C#/.NET projects. Shared by `CSharpPackage`
/// and `CSharpWorkspace` so a single edit here updates both trait impls.
///
/// `dotnet nuget push` has no native `--dry-run` mode, so
/// `default_dry_run_publish_command` returns `None` in both impls and the
/// actual dry-run flow lives in the RAII-managed `dry_run_publish`
/// override (`crate::dry_run::resolve_and_run_dry_run`).
pub(crate) const PUBLISH_COMMAND: &str = "dotnet pack -c Release && dotnet nuget push";
