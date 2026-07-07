//! # changepacks-utils
//!
//! Shared utilities for the changepacks system, consumed by every
//! language-specific crate (`changepacks-node`, `-python`, `-rust`,
//! `-dart`, `-csharp`, `-java`) and by the CLI commands themselves.
//!
//! ## Provided functionality
//!
//! - **Git repository discovery / walk** — [`find_current_git_repo`] locates
//!   the enclosing repo, [`find_project_dirs`] walks the git tree to discover
//!   every recognized manifest, and [`get_relative_path`] normalizes a path
//!   against the repo root. The concrete [`ThreadSafeRepository`] handle is
//!   re-exported so downstream crates (e.g. `changepacks-cli`) can cache it
//!   without a direct `gix` dep.
//! - **Workspace-by-sibling detection** — [`is_workspace_by_sibling`] decides
//!   whether a discovered manifest roots a workspace, either from an
//!   in-manifest field or a fixed sibling file (`pnpm-workspace.yaml`,
//!   `melos.yaml`), shared by the Node and Dart finders.
//! - **Semver arithmetic** — [`next_version`] applies an `UpdateType` bump
//!   to a version string; [`next_version_or_default`] wraps it with a
//!   `0.0.0` fallback for the unversioned-manifest case shared across every
//!   language crate's `update_version`.
//! - **Semver prefix split** — [`split_version`] cleaves a range specifier
//!   (`^`, `~`, `>=`, `helloworld-`) from the numeric tail so callers can
//!   rebuild `"<prefix><new_version>"` while preserving the prefix.
//! - **Dependency ordering** — [`sort_by_dependencies`] runs Kahn's
//!   algorithm over the project graph so publish walks touch dependencies
//!   before dependents.
//! - **Reverse-dep DFS + updateOn rules** — [`gen_update_map`] materializes
//!   the `changepack_log_*.json` → `(UpdateType, notes)` map for every
//!   changed package (including `updateOn` triggers), and
//!   [`apply_reverse_dependencies`] extends it with transitive PATCH bumps
//!   for every workspace member depending on a scheduled update.
//! - **Changepack log lifecycle** — [`clear_update_logs`] retires consumed
//!   `changepack_log_*.json` files after a successful `update`;
//!   [`gen_changepack_result_map`] aggregates per-project `(UpdateType,
//!   ChangePackResultLog)` for display / JSON output.
//! - **Format-preservation helpers** — [`detect_indent_str`] recovers the
//!   indent width/character of the on-disk JSON so `serde_json` roundtrips
//!   don't reformat it; the crate-internal `trailing_newline` helper reports
//!   the trailing-newline convention; [`finalize_content`] rebuilds output
//!   that matches the original file's trailing-whitespace shape byte-for-byte.
//! - **Result / progress display** — [`display_update`] renders the
//!   per-project update summary emitted by `changepacks update` / `check`.
//! - **Config + directory management** — [`get_changepacks_config`] and
//!   [`get_changepacks_config_at`] load `.changepacks/config.json` (with
//!   sensible defaults for `ignore` / `baseBranch` / `publish` / `updateOn`);
//!   [`get_changepacks_dir`] resolves the `.changepacks` directory path
//!   from the git repository root.

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
mod is_workspace_by_sibling;
mod next_version;
mod sort_by_dep;
mod split_version;
#[cfg(test)]
mod test_support;
mod trailing_newline;

pub(crate) use is_changepack_log::collect_changepack_log_paths;

// Re-export the concrete `gix` handle type so downstream crates (e.g.
// `changepacks-cli`) can hold onto it (e.g. caching on `CommandContext`)
// without taking a direct dependency on `gix` — mirrors how utils already
// wraps every other gix touch point.
pub use gix::ThreadSafeRepository;

pub use clear_update_logs::{clear_applied_update_logs, clear_update_logs};
pub use detect_indent::detect_indent_str;
pub use display_update::display_update;
pub use filter_project_dirs::find_project_dirs;
pub use find_current_git_repo::find_current_git_repo;
pub use gen_changepack_result_map::gen_changepack_result_map;
pub use gen_update_map::{apply_reverse_dependencies, gen_update_map};
pub use get_changepacks_config::{get_changepacks_config, get_changepacks_config_at};
pub use get_changepacks_dir::get_changepacks_dir;
pub use get_relative_path::get_relative_path;
pub use is_workspace_by_sibling::is_workspace_by_sibling;
pub use next_version::{next_version, next_version_or_default};
pub use sort_by_dep::sort_by_dependencies;
pub use split_version::{replace_version_keep_prefix, split_version};
pub use trailing_newline::finalize_content;
