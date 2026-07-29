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
use changepacks_utils::{read_and_parse, replace_version_keep_prefix, write_finalized};
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

/// Read and parse a Cargo.toml file, preserving the raw content for format finalization.
///
/// Returns both the raw file content and the parsed `DocumentMut` to enable
/// [`write_finalized`] to preserve formatting, comments, and the complete
/// trailing-whitespace suffix.
///
/// The read-then-parse-with-context sequence lives in
/// [`changepacks_utils::read_and_parse`] — the mirror of [`write_finalized`] —
/// so only the `Cargo.toml` label and the `toml_edit` parser stay here.
///
/// # Errors
/// Returns error if the file cannot be read or the TOML cannot be parsed.
pub(crate) async fn read_and_parse_cargo_toml(path: &Path) -> Result<(String, DocumentMut)> {
    read_and_parse(path, "Cargo.toml", str::parse::<DocumentMut>).await
}

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
/// An empty `[package]` table is created if missing. The explicit
/// `Table::new()` matters: plain `doc["package"]["version"] = ...`
/// auto-creates an INLINE table (`package = { version = ... }`) at the top
/// of the document instead of a proper `[package]` header — the same hazard
/// guarded in `changepacks-python`'s `write_pyproject_version`.
///
/// The version assignment goes through [`assign_preserving_decor`] so an
/// end-of-line comment on the version line survives the bump.
///
/// # Errors
/// Returns error if the file cannot be read, the TOML cannot be parsed,
/// or the write fails.
pub(crate) async fn write_cargo_package_version(path: &Path, new_version: &str) -> Result<()> {
    let (cargo_toml_raw, mut cargo_toml) = read_and_parse_cargo_toml(path).await?;
    if !ensure_package_table_like(&cargo_toml, path)? {
        cargo_toml["package"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    assign_preserving_decor(&mut cargo_toml["package"]["version"], new_version);
    write_finalized(path, cargo_toml.to_string(), &cargo_toml_raw, "Cargo.toml").await
}

/// Reject a `Cargo.toml` whose top-level `package` key exists but is NOT
/// table-like (e.g. `package = 3` or `package = "not-a-table"`), and report
/// whether the key is present at all.
///
/// Both writers that materialize `[package]` need the SAME two facts before
/// touching the document — "is the existing `package` item safe to index
/// into?" and "does it already exist?" — and both previously open-coded the
/// identical `is_some_and(|package| !package.is_table_like())` check plus a
/// byte-identical `bail!`: once in [`write_cargo_package_version`] above and
/// once in [`crate::workspace::RustWorkspace::update_version`]. Extracted here
/// beside [`is_workspace_marker`] and [`workspace_dependencies_table_mut`] so
/// the manifest-shape assumption AND its user-visible message live in ONE
/// place, matching the repo-wide "one decoder, one place" convention.
///
/// Callers reuse the returned flag for their own control flow — creating the
/// missing `[package]` table in the package writer, or driving the
/// hybrid/virtual-root branch in the workspace writer — so the extraction adds
/// no extra lookup relative to the previous hand-rolled pairs.
///
/// Guarding BEFORE any mutation is the point: `toml_edit` indexing assignment
/// would otherwise silently replace the scalar and rewrite the manifest.
///
/// The body itself now lives in
/// [`changepacks_utils::ensure_toml_table_like`], because
/// `changepacks-python`'s `ensure_project_table_like` was the same function
/// modulo the key name and the manifest label, and `crates/AGENTS.md` forbids
/// importing one language crate into another. This wrapper stays so the
/// `Cargo.toml`-specific key/label pair is bound in ONE place and every call
/// site inside this crate is unchanged.
///
/// # Errors
/// Returns an error naming `path` when `package` is present but not table-like.
pub(crate) fn ensure_package_table_like(doc: &DocumentMut, path: &Path) -> Result<bool> {
    changepacks_utils::ensure_toml_table_like(doc, path, "package", "Cargo.toml")
}

/// Overwrite `slot` with the string `new_value`, carrying the previous value's
/// [`toml_edit::Decor`] across the assignment.
///
/// Every `Cargo.toml` version rewrite replaces a whole [`toml_edit::Item`] with
/// a freshly built one, and a fresh value carries DEFAULT (empty) decor. Decor
/// is where `toml_edit` keeps the trivia around a value — the spacing after
/// `=` and, most visibly, an end-of-line comment such as
/// `version = "1.2.3" # pinned by release tooling`. Without this capture and
/// restore, a routine version bump silently deletes that comment from the
/// user's manifest, which is exactly the format-preservation guarantee this
/// tool advertises.
///
/// `Cargo.toml` has SIX such write sites (the package writer here plus five in
/// [`crate::workspace::RustWorkspace`]), so the policy is factored out to ONE
/// place beside [`ensure_package_table_like`] and [`is_workspace_marker`],
/// matching the repo-wide "one decoder, one place" convention.
///
/// A slot that does not currently hold a value — a missing key auto-vivified
/// by `toml_edit` indexing, or a table — has no decor to preserve, so the
/// restore is skipped and behaviour is identical to a plain assignment.
///
/// The body itself now lives in
/// [`changepacks_utils::assign_preserving_decor`], because
/// `changepacks-python`'s `write_pyproject_version` open-coded the identical
/// capture / assign / restore triple inline, and `crates/AGENTS.md` forbids
/// importing one language crate into another — the same reasoning that moved
/// [`ensure_package_table_like`]'s body there. This wrapper stays so every
/// call site inside this crate is unchanged.
pub(crate) fn assign_preserving_decor(slot: &mut toml_edit::Item, new_value: &str) {
    changepacks_utils::assign_preserving_decor(slot, new_value);
}

/// Rewrite the `version` of a `[workspace.dependencies]` entry that also
/// carries a `path`, keeping the entry's range prefix and its `toml_edit`
/// decor, and report whether anything was written.
///
/// Both `[workspace.dependencies]` fan-outs in
/// [`crate::workspace::RustWorkspace`] — the workspace-version sync in
/// `update_version` and the member-version sync in
/// `update_workspace_dependencies` — previously open-coded the SAME five
/// steps: match `as_table_like_mut` (which accepts BOTH inline-table deps
/// `foo = { path = "...", version = "..." }` AND sub-table deps
/// `[workspace.dependencies.foo]`, while string deps `foo = "1.0"` yield
/// `None` and are skipped), require an existing `path` key, read the current
/// `version` as a string, decide whether this entry is in scope, then rewrite
/// it. Only the decision differs, so it is injected as `accept` and everything
/// else lives here — matching the repo-wide "one decoder, one place"
/// convention already followed by [`assign_preserving_decor`] and
/// [`workspace_dependencies_table_mut`].
///
/// The owned bumped string is built via
/// [`changepacks_utils::replace_version_keep_prefix`] BEFORE the
/// `get_mut("version")` mutable borrow is taken, so no shared borrow of the
/// dependency table outlives it. `TableLike` exposes no `Index`/`[]` operator,
/// so the value is rewritten in place through `get_mut` — a `version` key that
/// does not already exist is NEVER inserted.
///
/// Returns `false` — writing nothing — when the item is not table-like, has no
/// `path`, has no string `version`, or `accept` rejects the current specifier.
pub(crate) fn sync_path_dependency_version(
    value: &mut toml_edit::Item,
    next_version: &str,
    accept: impl FnOnce(&str) -> bool,
) -> bool {
    let Some(dep) = value.as_table_like_mut() else {
        return false;
    };
    if dep.get("path").is_none() {
        return false;
    }
    let Some(current_version) = dep.get("version").and_then(|v| v.as_str()) else {
        return false;
    };
    if !accept(current_version) {
        return false;
    }
    let bumped = replace_version_keep_prefix(current_version, next_version);
    let Some(slot) = dep.get_mut("version") else {
        return false;
    };
    assign_preserving_decor(slot, &bumped);
    true
}

/// Return `true` for a `toml_edit::Item` whose value is table-like with
/// `workspace = true` — the shape Cargo uses to mark either a
/// `[dependencies]` entry as inheriting from `[workspace.dependencies]`
/// (`dep = { workspace = true }`) or a `[package]` scalar as inheriting
/// from `[workspace.package]` (`version.workspace = true`, which `toml_edit`
/// parses as a dotted-key table `version = { workspace = true }`).
///
/// Shared by [`finder`](crate::finder) (`workspace_dep_names` and the
/// `inherits_workspace` chain in `visit`) and [`workspace`](crate::workspace)
/// (`RustWorkspace::update_version`'s hybrid-root inheritance guard) so the
/// "is this an inherited-version / inherited-dep marker" decision lives in ONE
/// place — matching the repo-wide "one decoder, one place" convention
/// (`is_regular_file`, `should_mark_changed`, `lookup_by_path_or_language`, …).
/// Byte-identical semantics to the previous hand-rolled chains:
/// `as_table_like()` returns `None` for scalars, `.get("workspace")` returns
/// `None` when the key is missing, `.as_bool()` returns `None` for non-bool
/// values, and each `None` path collapses to `false` via `.unwrap_or(false)`.
#[must_use]
pub(crate) fn is_workspace_marker(item: &toml_edit::Item) -> bool {
    item.as_table_like()
        .and_then(|t| t.get("workspace"))
        .and_then(|w| w.as_bool())
        .unwrap_or(false)
}

/// Look up the `[workspace.dependencies]` table for mutation, mirroring the
/// `doc.get_mut("workspace").and_then(|w| w.get_mut("dependencies")).and_then(|d| d.as_table_mut())`
/// chain that was previously open-coded twice in
/// [`workspace`](crate::workspace): once in `RustWorkspace::update_version`
/// (the workspace-version fan-out into path deps) and once in
/// `RustWorkspace::update_workspace_dependencies` (the member-version sync).
/// Extracted so the manifest-shape assumption lives in ONE place, matching the
/// precedent set by `workspace_package_str` in [`finder`](crate::finder).
///
/// Returns `None` on any missing hop — no `[workspace]`, no
/// `[workspace.dependencies]`, or a non-table `dependencies` item — leaving
/// each caller free to keep its own control flow (`if let Some(..)` vs a
/// `let ... else { return Ok(()) }`). Returning the borrowed `toml_edit`
/// handle rather than an owned copy is what keeps formatting, indentation, and
/// key order untouched.
pub(crate) fn workspace_dependencies_table_mut(
    doc: &mut DocumentMut,
) -> Option<&mut toml_edit::Table> {
    doc.get_mut("workspace")
        .and_then(|w| w.get_mut("dependencies"))
        .and_then(|d| d.as_table_mut())
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_cargo_package_version_preserves_complete_trailing_whitespace() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // A gnarly suffix (space, tab, CRLF, space, LF) that a plain
        // `DocumentMut::to_string()` would collapse — only `finalize_content`
        // restores it byte for byte.
        let suffix = " \t\r\n \n";
        fs::write(
            &cargo_toml,
            format!("[package]\nname = \"x\"\nversion = \"1.0.0\"{suffix}"),
        )
        .unwrap();

        write_cargo_package_version(&cargo_toml, "2.0.0")
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&cargo_toml).unwrap(),
            format!("[package]\nname = \"x\"\nversion = \"2.0.0\"{suffix}")
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, "[package]\nversion = \"1.0.0\"\n").unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        test_support::set_readonly(&cargo_toml, true);

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result = write_cargo_package_version(&cargo_toml, "2.0.0").await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        test_support::set_readonly(&cargo_toml, false);

        let err = result.expect_err("write to a readonly Cargo.toml must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&cargo_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_non_table_package_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, "package = 3\n").unwrap();

        let err = write_cargo_package_version(&cargo_toml, "2.0.0")
            .await
            .expect_err("non-table package item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&cargo_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("non-table [package]"),
            "error chain should mention the non-table package item, got: {chain}"
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_non_table_package_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // A scalar top-level `package` key. The sibling test above pins the
        // ERROR TEXT; this one pins the guard's actual reason for existing —
        // it must reject BEFORE the `cargo_toml["package"]["version"] = ...`
        // assignment ever runs, so the manifest on disk is never clobbered.
        let original = "package = \"not-a-table\"\n\n[dependencies]\nserde = \"1\"\n";
        fs::write(&cargo_toml, original).unwrap();

        let err = write_cargo_package_version(&cargo_toml, "1.0.1")
            .await
            .expect_err("non-table package item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("has a non-table [package] item"),
            "error chain should name the non-table package guard, got: {chain}"
        );

        // Byte-for-byte, not line-for-line: a partial or reformatted write is
        // exactly the manifest destruction the guard prevents.
        assert_eq!(
            fs::read(&cargo_toml).unwrap(),
            original.as_bytes(),
            "a rejected bump must leave the manifest byte-identical"
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_creates_proper_package_header() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // A manifest with NO [package] table at all. Without the explicit
        // `Table::new()` guard, `doc["package"]["version"] = ...` auto-creates
        // an INLINE table (`package = { version = "0.0.1" }`) at the top of
        // the document instead of a proper `[package]` header.
        fs::write(&cargo_toml, "[workspace]\nmembers = [\"a\"]\n").unwrap();

        write_cargo_package_version(&cargo_toml, "0.0.1")
            .await
            .unwrap();

        let written = fs::read_to_string(&cargo_toml).unwrap();
        assert!(
            written.lines().any(|line| line.trim() == "[package]"),
            "output must contain a literal [package] header line, got: {written}"
        );
        assert!(
            written.contains("version = \"0.0.1\""),
            "output must contain the new version, got: {written}"
        );
        assert!(
            !written.contains("package = {"),
            "output must not use the inline-table form, got: {written}"
        );
        assert!(
            written.lines().any(|line| line.trim() == "[workspace]")
                && written.contains("members = [\"a\"]"),
            "output must preserve the existing [workspace] section, got: {written}"
        );
    }

    #[tokio::test]
    async fn test_write_cargo_package_version_preserves_version_line_decor() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // The version line carries an end-of-line comment, which `toml_edit`
        // stores as the VALUE's suffix decor. Assigning a freshly built value
        // would drop it, silently deleting the user's comment on a routine
        // bump. Whole-file round trip: only the version literal may change.
        let original = "# release-managed manifest\n[package]\nname = \"x\"\nversion = \"1.0.0\" # pinned by release tooling\nedition = \"2024\"\n";
        fs::write(&cargo_toml, original).unwrap();

        write_cargo_package_version(&cargo_toml, "2.0.0")
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&cargo_toml).unwrap(),
            original.replace("1.0.0", "2.0.0"),
            "only the version literal may change"
        );
    }
}
