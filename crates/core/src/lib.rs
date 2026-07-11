//! # changepacks-core
//!
//! Core traits, types, and interfaces for the changepacks version management system.
//!
//! This crate defines the fundamental abstractions used across all language-specific
//! implementations. The main traits are `Package` for single projects, `Workspace` for
//! monorepo roots, and `ProjectFinder` for discovering projects in a git tree.

mod change_detection;
mod changepack_result;
mod config;
mod language;
mod package;
mod project;
mod project_finder;
pub mod publish;
mod publish_result;
mod update_log;
mod update_type;
mod workspace;

#[cfg(test)]
pub(crate) mod test_support;

// Re-export traits for convenience
pub use change_detection::contains_changepacks_component;
pub use changepack_result::{ChangePackResult, ChangePackResultLog};
pub use config::Config;
pub use language::Language;
pub use package::Package;
pub use project::{Project, format_version_display};
pub use project_finder::{ProjectFinder, is_regular_file};
pub use publish::PublishOutput;
pub use publish_result::PublishResult;
pub use update_log::ChangePackLog;
pub use update_type::UpdateType;
pub use workspace::Workspace;
