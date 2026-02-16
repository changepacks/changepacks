//! # changepacks-csharp
//!
//! C#/.NET project support for changepacks.
//!
//! Implements project discovery and version management for .csproj XML files. Uses quick-xml
//! for parsing with format preservation. Supports MSBuild project files with version elements
//! and handles both single projects and multi-project solutions.

pub mod finder;
pub mod package;
pub mod workspace;
mod xml_utils;

pub use finder::CSharpProjectFinder;
