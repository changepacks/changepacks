//! # changepacks-rust
//!
//! Rust project support for changepacks.
//!
//! Implements project discovery and version management for Cargo.toml files. Uses `toml_edit`
//! for non-destructive parsing to preserve file formatting, comments, and whitespace. Handles
//! both single crates and Cargo workspace configurations.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::RustProjectFinder;

use std::path::Path;

use anyhow::Result;
use changepacks_utils::trailing_newline;
use tokio::fs::{read_to_string, write};
use toml_edit::DocumentMut;

/// Default publish command for a single-crate `Cargo.toml`.
///
/// Kept as a `pub(crate) const` here so `RustPackage::default_publish_command`
/// and `RustWorkspace::default_publish_command` (which each have their own
/// workspace-scoped variant below) reference ONE source of truth. Every
/// other language crate — `changepacks-python`, `changepacks-dart`,
/// `changepacks-java`, `changepacks-csharp` — already exposes this same
/// `PUBLISH_COMMAND` const; this fills the Rust-crate gap so the pattern is
/// uniform and a future edit lives in one place.
pub(crate) const PUBLISH_COMMAND: &str = "cargo publish";

/// Default dry-run publish command for a single-crate `Cargo.toml`.
///
/// Paired with `PUBLISH_COMMAND` so both live next to each other for
/// package-scope callers.
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = "cargo publish --dry-run";

/// Default publish command for a Cargo workspace root.
///
/// `--workspace` publishes every member in one invocation, which matches
/// what `RustWorkspace::default_publish_command` returned before this
/// consolidation.
pub(crate) const WORKSPACE_PUBLISH_COMMAND: &str = "cargo publish --workspace";

/// Default dry-run publish command for a Cargo workspace root.
///
/// Paired with `WORKSPACE_PUBLISH_COMMAND` for the workspace-scope callers.
pub(crate) const WORKSPACE_DRY_RUN_PUBLISH_COMMAND: &str = "cargo publish --workspace --dry-run";

/// Update the `[package].version` key of the `Cargo.toml` at `path` to
/// `new_version`, using `toml_edit` to preserve the file's formatting,
/// comments, and trailing-newline shape.
///
/// This helper only handles the simple `[package].version = "X.Y.Z"` case
/// used by [`crate::package::RustPackage`]. Workspace roots — where the
/// bump lives in `[workspace.package].version` and must also fan out into
/// `[workspace.dependencies]` path deps and virtual-workspace tables —
/// stay in [`crate::workspace::RustWorkspace::update_version`] because
/// they need much more than a single key rewrite.
///
/// Shared by future paths that need the same skeleton so a single edit
/// here keeps the format-preservation invariants in one place — matching
/// the Node/Python/Dart/CSharp convention documented in
/// `crates/AGENTS.md`.
///
/// # Errors
/// Returns error if the file cannot be read, the TOML cannot be parsed,
/// or the write fails.
pub(crate) async fn write_cargo_package_version(path: &Path, new_version: &str) -> Result<()> {
    let cargo_toml_raw = read_to_string(path).await?;
    let mut cargo_toml: DocumentMut = cargo_toml_raw.parse::<DocumentMut>()?;
    cargo_toml["package"]["version"] = new_version.into();
    write(
        path,
        format!(
            "{}{}",
            cargo_toml.to_string().trim_end(),
            trailing_newline(&cargo_toml_raw)
        ),
    )
    .await?;
    Ok(())
}
