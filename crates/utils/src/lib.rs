//! # changepacks-utils
//!
//! Shared utilities for the changepacks system.
//!
//! Provides git repository operations via gix, version calculation, dependency sorting with
//! Kahn's algorithm, config management, and format detection for JSON indentation. These
//! utilities are used across all language-specific crates and CLI commands.

mod clear_update_logs;
mod detect_indent;
mod display_update;
mod filter_project_dirs;
mod find_current_git_repo;
mod gen_changepack_result_map;
mod gen_update_map;
mod get_changepacks_config;
mod get_changepacks_dir;
mod get_relative_path;
mod is_changepack_log;
mod next_version;
mod sort_by_dep;
mod split_version;
mod trailing_newline;

pub(crate) use is_changepack_log::is_changepack_log_json_name;

// Re-export the concrete `gix` handle type so downstream crates (e.g.
// `changepacks-cli`) can hold onto it (e.g. caching on `CommandContext`)
// without taking a direct dependency on `gix` — mirrors how utils already
// wraps every other gix touch point.
pub use gix::ThreadSafeRepository;

pub use clear_update_logs::clear_update_logs;
pub use detect_indent::detect_indent_str;
pub use display_update::display_update;
pub use filter_project_dirs::find_project_dirs;
pub use find_current_git_repo::find_current_git_repo;
pub use gen_changepack_result_map::gen_changepack_result_map;
pub use gen_update_map::{apply_reverse_dependencies, gen_update_map};
pub use get_changepacks_config::get_changepacks_config;
pub use get_changepacks_dir::get_changepacks_dir;
pub use get_relative_path::get_relative_path;
pub use next_version::next_version;
pub use sort_by_dep::sort_by_dependencies;
pub use split_version::split_version;
pub use trailing_newline::trailing_newline;
