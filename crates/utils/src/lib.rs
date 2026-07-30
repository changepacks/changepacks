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
//!   against the repo root. [`find_current_git_repo`] returns a
//!   [`ThreadSafeRepository`] and [`find_project_dirs`] borrows one, so that
//!   concrete `gix` handle type is re-exported for downstream crates (e.g.
//!   `changepacks-cli`) to name without a direct `gix` dep.
//! - **Workspace-by-sibling detection** — [`is_workspace_by_sibling`] decides
//!   whether a discovered manifest roots a workspace, either from an
//!   in-manifest field or a fixed sibling file (`pnpm-workspace.yaml`,
//!   `melos.yaml`), shared by the Node and Dart finders.
//! - **Optional-declaration shape guard** — [`ensure_declared_shape`] turns a
//!   caller-evaluated `Option<bool>` into "declared / not declared", rejecting a
//!   present-but-wrongly-shaped field with the single
//!   ``Invalid `<field>` declaration in <path>: expected <expected>`` template
//!   the Node, Dart and Python finders each open-coded for their workspace
//!   declaration.
//! - **Semver arithmetic** — [`next_version`] applies an `UpdateType` bump
//!   to a version string; [`next_version_or_default`] wraps it with a
//!   `0.0.0` fallback for the unversioned-manifest case shared across every
//!   language crate's `update_version`; [`bump_version_with`] is the shared
//!   compute-write-store helper that all six language crates use from their
//!   `update_version` to atomically bump a manifest's version field.
//! - **Semver prefix split** — [`split_version`] cleaves a range specifier
//!   (`^`, `~`, `>=`, `helloworld-`) from the numeric tail so callers can
//!   rebuild `"<prefix><new_version>"` while preserving the prefix.
//! - **Dependency ordering** — [`sort_by_dependencies`] runs Kahn's
//!   algorithm over the project graph so publish walks touch dependencies
//!   before dependents.
//! - **Name resolution** — [`ProjectNameAnalysis`] is the single index from a
//!   dependency name to the unique project providing it (or
//!   [`ProjectNameResolution::Ambiguous`] when several share the name), shared
//!   by [`sort_by_dependencies`], [`apply_reverse_dependencies`] and the CLI
//!   `check --tree` renderer.
//! - **Reverse-dep DFS + updateOn rules** — [`gen_update_map`] materializes
//!   the `changepack_log_*.json` → `(UpdateType, notes)` map for every
//!   changed package (including `updateOn` triggers), and
//!   [`apply_reverse_dependencies`] extends it with transitive PATCH bumps
//!   for every workspace member depending on a scheduled update.
//! - **Changepack log lifecycle** — [`clear_update_logs`] retires consumed
//!   `changepack_log_*.json` files after a successful `update`;
//!   [`gen_changepack_result_map`] aggregates per-project `(UpdateType,
//!   ChangePackResultLog)` for display / JSON output.
//! - **Manifest read + parse head** — [`read_and_parse`] is the shared head of
//!   every language crate's manifest pipeline and the exact mirror of
//!   [`write_finalized`] below: read the file, hand its text to a
//!   caller-supplied parser, and attach `Failed to read <label> <path>` /
//!   `Failed to parse <label> <path>` contexts, returning the raw text next to
//!   the parsed value.
//! - **TOML manifest shape guard** (optional `toml` feature) —
//!   `ensure_toml_table_like` rejects a manifest whose top-level table key
//!   (`[package]` in `Cargo.toml`, `[project]` in `pyproject.toml`) holds a
//!   non-table scalar, and reports whether the key exists, so the Rust and
//!   Python writers share ONE guard instead of two mirrored copies.
//! - **TOML decor-preserving assignment** (optional `toml` feature) —
//!   `assign_preserving_decor` overwrites a value slot while carrying the old
//!   value's `toml_edit::Decor` across, so an end-of-line comment or unusual
//!   spacing on a `version = "…"` line survives a bump; shared by every
//!   `Cargo.toml` write site and by `pyproject.toml`'s `[project].version`.
//! - **TOML `[table].version` writer** (optional `toml` feature) —
//!   `write_toml_table_version` holds the whole read → table-like guard →
//!   caller validation → table creation → decor-preserving assign →
//!   trailing-whitespace-preserving write pipeline that `changepacks-rust`'s
//!   `write_cargo_package_version` and `changepacks-python`'s
//!   `write_pyproject_version` previously open-coded twice, leaving each crate
//!   with only its manifest label, table key, and extra validation rule.
//! - **Format-preservation helpers** — [`detect_indent_str`] recovers the
//!   indent width/character of the on-disk JSON so `serde_json` roundtrips
//!   don't reformat it; [`write_finalized`] is the shared manifest-write tail —
//!   rebuild the body so it matches the original file's trailing-whitespace
//!   shape byte-for-byte, write it, and attach a `Failed to write <label>
//!   <path>` context — used by every language crate's manifest rewriter.
//! - **Result / progress display** — [`display_update`] renders the
//!   per-project update summary emitted by `changepacks update` / `check`.
//! - **Config + directory management** — [`get_changepacks_config`] and
//!   [`get_changepacks_config_at`] load `.changepacks/config.json` (with
//!   sensible defaults for `ignore` / `baseBranch` / `publish` / `updateOn`);
//!   [`get_changepacks_dir`] resolves the `.changepacks` directory path
//!   from the git repository root.

mod applied_change_spans;
#[cfg(feature = "toml")]
mod assign_preserving_decor;
mod bump_version_with;
mod clear_update_logs;
mod detect_indent;
mod display_update;
mod ensure_declared_shape;
#[cfg(feature = "toml")]
mod ensure_toml_table_like;
mod find_current_git_repo;
mod find_project_dirs;
mod gen_changepack_result_map;
mod gen_update_map;
mod get_changepacks_config;
mod get_changepacks_dir;
mod get_relative_path;
mod is_changepack_log;
mod is_workspace_by_sibling;
mod next_version;
mod project_names;
mod read_and_parse;
mod sort_by_dep;
mod split_version;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod trailing_newline;
#[cfg(feature = "toml")]
mod write_toml_table_version;

pub(crate) use is_changepack_log::read_log_bodies;

// Re-export the concrete `gix` handle type that this crate's own signatures
// expose: `find_current_git_repo` returns a `ThreadSafeRepository` and
// `find_project_dirs` takes one by reference. Re-exporting it lets downstream
// crates (e.g. `changepacks-cli`) name and pass those values without taking a
// direct dependency on `gix` — mirrors how utils already wraps every other
// gix touch point.
pub use gix::ThreadSafeRepository;

#[cfg(feature = "toml")]
pub use assign_preserving_decor::assign_preserving_decor;
pub use bump_version_with::bump_version_with;
pub use clear_update_logs::{clear_applied_update_logs, clear_update_logs};
pub use detect_indent::detect_indent_str;
pub use display_update::display_update;
pub use ensure_declared_shape::ensure_declared_shape;
#[cfg(feature = "toml")]
pub use ensure_toml_table_like::ensure_toml_table_like;
pub use find_current_git_repo::find_current_git_repo;
pub use find_project_dirs::find_project_dirs;
pub use gen_changepack_result_map::gen_changepack_result_map;
pub use gen_update_map::{
    CARRY_FORWARD_LOG_PREFIX, UpdatePlan, apply_reverse_dependencies, gen_update_map,
};
pub use get_changepacks_config::{get_changepacks_config, get_changepacks_config_at};
pub use get_changepacks_dir::get_changepacks_dir;
pub use get_relative_path::{get_relative_path, get_relative_path_ref};
pub use is_changepack_log::collect_changepack_log_paths;
pub use is_workspace_by_sibling::is_workspace_by_sibling;
pub use next_version::{next_version, next_version_or_default};
pub use project_names::{ProjectNameAnalysis, ProjectNameResolution};
pub use read_and_parse::read_and_parse;
pub use sort_by_dep::{
    DependencyAmbiguityError, DependencyCycleError, DependencyCycleMember, DependencySortError,
    sort_by_dependencies,
};
pub use split_version::{replace_version_keep_prefix, split_version};
pub use trailing_newline::write_finalized;
#[cfg(feature = "toml")]
pub use write_toml_table_version::write_toml_table_version;
