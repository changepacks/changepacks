//! # changepacks-python
//!
//! Python project support for changepacks.
//!
//! Implements project discovery and version management for pyproject.toml files. Parses
//! TOML using `toml_edit` for non-destructive formatting preservation when updating
//! versions. Supports both single packages and workspace configurations.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::PythonProjectFinder;

/// Default publish command for Python projects. Shared by `PythonPackage`
/// and `PythonWorkspace` so a single edit here updates both trait impls.
pub(crate) const PUBLISH_COMMAND: &str = "uv publish";

/// Default dry-run publish command for Python projects.
/// `uv publish --dry-run` is `uv`'s built-in non-mutating verification;
/// users can override via `publishDryRun` in `.changepacks/config.json`.
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = "uv publish --dry-run";

use std::path::Path;

use anyhow::Result;
use changepacks_core::UpdateType;
use changepacks_utils::{assign_preserving_decor, read_and_parse, write_finalized};
use toml_edit::DocumentMut;

/// Compute the next version for `update_type`, write it into the
/// `pyproject.toml` at `path`, and store it back into `version`.
///
/// `PythonPackage::update_version` and `PythonWorkspace::update_version` had
/// byte-identical bodies; this holds that body once so the two trait impls
/// cannot drift. Both `update_version` signatures stay hand-written rather
/// than macro-generated: `async_trait` rewrites the `impl` block before a
/// `macro_rules!` body expands, so a macro would emit a plain `async fn` that
/// no longer matches the desugared trait signature (E0195) — the same reason
/// documented at `crates/java/src/package.rs:65-74`.
///
/// # Errors
/// Returns an error when semver calculation fails or the manifest write fails.
pub(crate) async fn bump_pyproject_version(
    version: &mut Option<String>,
    path: &Path,
    update_type: UpdateType,
) -> Result<()> {
    changepacks_utils::bump_version_with(version, path, update_type, async |new| {
        crate::write_pyproject_version(path, new).await
    })
    .await
}

/// Read and parse a pyproject.toml file, returning both the raw content
/// (for trailing-newline preservation) and the parsed TOML document.
///
/// The read-then-parse-with-context sequence lives in
/// [`changepacks_utils::read_and_parse`] — the mirror of [`write_finalized`] —
/// so only the `pyproject.toml` label and the `toml_edit` parser stay here.
///
/// # Errors
/// Returns error if the file cannot be read or is not valid TOML.
pub(crate) async fn read_and_parse_pyproject_toml(path: &Path) -> Result<(String, DocumentMut)> {
    read_and_parse(path, "pyproject.toml", str::parse::<DocumentMut>).await
}

/// Update `pyproject.toml` at `path` to set `[project].version` to
/// `new_version`, preserving the file's complete trailing-whitespace shape
/// (via `write_finalized`) and its TOML formatting (via `toml_edit`).
///
/// Shared by `PythonPackage::update_version` and
/// `PythonWorkspace::update_version` so both paths emit byte-identical output.
/// An empty `[project]` table is created if missing — needed for workspace
/// roots that only declare `[tool.uv.workspace]` and for `[build-system]`-only
/// package manifests (a valid PEP 517 shape). The explicit `Table::new()`
/// matters: plain `doc["project"]["version"] = ...` auto-creates an INLINE
/// table (`project = { version = ... }`) at the top of the document instead
/// of a proper `[project]` header.
///
/// The version assignment goes through
/// [`changepacks_utils::assign_preserving_decor`], which carries the existing
/// value's [`toml_edit::Decor`] across the write. Assigning a freshly built
/// value replaces the whole `Item`, and a fresh value carries default (empty)
/// decor, so without that the surrounding trivia — most visibly an
/// end-of-line comment such as `version = "1.2.3" # pinned` — would be
/// silently deleted from the user's manifest by a routine version bump. The
/// helper is shared with `changepacks-rust`'s six `Cargo.toml` write sites
/// instead of being open-coded here.
///
/// # Errors
/// Returns error if the file cannot be read, is not valid TOML, or the write
/// fails.
pub(crate) async fn write_pyproject_version(path: &Path, new_version: &str) -> Result<()> {
    let (pyproject_toml_raw, mut pyproject_toml) = read_and_parse_pyproject_toml(path).await?;
    let has_project = ensure_project_table_like(&pyproject_toml, path)?;
    // The borrow ends at its last use, so the mutations that follow still
    // type-check without re-walking the document.
    let has_dynamic_version = pyproject_toml
        .get("project")
        .and_then(|project| project.get("dynamic"))
        .and_then(toml_edit::Item::as_array)
        .is_some_and(|dynamic| dynamic.iter().any(|item| item.as_str() == Some("version")));
    if has_dynamic_version {
        anyhow::bail!(
            "pyproject.toml {} has backend-managed version in project.dynamic",
            path.display()
        );
    }
    if !has_project {
        pyproject_toml["project"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    // The indexed slot IS the value whose decor is preserved: when
    // `[project].version` exists it is that value, and when `[project]` was
    // just created — or exists without a `version` — `toml_edit` auto-vivifies
    // a non-value `Item`, so there is no decor to carry and the helper falls
    // back to a plain assignment.
    assign_preserving_decor(&mut pyproject_toml["project"]["version"], new_version);
    write_finalized(
        path,
        pyproject_toml.to_string(),
        &pyproject_toml_raw,
        "pyproject.toml",
    )
    .await
}

/// Reject a `pyproject.toml` whose top-level `project` key exists but is NOT
/// table-like (e.g. `project = 3`), and report whether the key is present at
/// all.
///
/// [`write_pyproject_version`] needs the SAME two facts before touching the
/// document — "is the existing `project` item safe to index into?" and "does
/// it already exist?" — and previously answered them with two separate
/// `get("project")` walks. Folding both into one call keeps the
/// manifest-shape assumption AND its user-visible message in ONE place, and
/// the returned flag drives the missing-`[project]` creation so no extra
/// lookup is introduced.
///
/// Guarding BEFORE any mutation is the point: `toml_edit` indexing assignment
/// would otherwise silently replace the scalar and rewrite the manifest.
///
/// This used to be a hand-copied mirror of `changepacks_rust`'s
/// `ensure_package_table_like` — the same function modulo the key name and the
/// manifest label. `crates/AGENTS.md` forbids importing one language crate
/// into another, so the shared body now lives in
/// [`changepacks_utils::ensure_toml_table_like`] and both crates keep a thin
/// wrapper that binds their own key/label pair, leaving every call site
/// unchanged.
///
/// # Errors
/// Returns an error naming `path` when `project` is present but not table-like.
pub(crate) fn ensure_project_table_like(doc: &DocumentMut, path: &Path) -> Result<bool> {
    changepacks_utils::ensure_toml_table_like(doc, path, "project", "pyproject.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_pyproject_version_preserves_complete_trailing_whitespace() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        let suffix = " \t\r\n \n";
        fs::write(
            &pyproject_toml,
            format!("[project]\nversion = \"1.0.0\"{suffix}"),
        )
        .unwrap();

        write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&pyproject_toml).unwrap(),
            format!("[project]\nversion = \"2.0.0\"{suffix}")
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(&pyproject_toml, "[project]\nversion = \"1.0.0\"\n").unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        test_support::set_readonly(&pyproject_toml, true);

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result = write_pyproject_version(&pyproject_toml, "2.0.0").await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        test_support::set_readonly(&pyproject_toml, false);

        let err = result.expect_err("write to a readonly pyproject.toml must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_non_table_project_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(&pyproject_toml, "project = 3\n").unwrap();

        let err = write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .expect_err("non-table project item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("non-table [project]"),
            "error chain should mention the non-table project item, got: {chain}"
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_non_table_project_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        // A scalar top-level `project` key. The sibling test above pins the
        // ERROR TEXT; this one pins the guard's actual reason for existing —
        // it must reject BEFORE the `pyproject_toml["project"]["version"] = ...`
        // assignment ever runs, so the manifest on disk is never clobbered.
        let original = "project = 1\n\n[build-system]\nrequires = [\"hatchling\"]\n";
        fs::write(&pyproject_toml, original).unwrap();

        let err = write_pyproject_version(&pyproject_toml, "1.0.1")
            .await
            .expect_err("non-table project item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("has a non-table [project] item"),
            "error chain should name the non-table project guard, got: {chain}"
        );
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );

        // Byte-for-byte, not line-for-line: a partial or reformatted write is
        // exactly the manifest destruction the guard prevents.
        assert_eq!(
            fs::read(&pyproject_toml).unwrap(),
            original.as_bytes(),
            "a rejected bump must leave the manifest byte-identical"
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_rejects_dynamic_version_multiline() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        let content = "[project]\ndynamic = [ \"version\" ]\n";
        fs::write(&pyproject_toml, content).unwrap();

        let err = write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .expect_err("dynamic version must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("has backend-managed version in project.dynamic"),
            "error chain should mention project.dynamic, got: {chain}"
        );

        let after = fs::read(&pyproject_toml).unwrap();
        assert_eq!(
            after,
            content.as_bytes(),
            "file bytes must be unchanged after rejection"
        );
    }

    #[tokio::test]
    async fn test_write_pyproject_version_rejects_dynamic_version_compact() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        let content = "[project]\ndynamic = [\"version\"]\n";
        fs::write(&pyproject_toml, content).unwrap();

        let err = write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .expect_err("dynamic version must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pyproject_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("project.dynamic"),
            "error chain should mention project.dynamic, got: {chain}"
        );

        let after = fs::read(&pyproject_toml).unwrap();
        assert_eq!(
            after,
            content.as_bytes(),
            "file bytes must be unchanged after rejection"
        );
    }

    /// Pins the boundary of the `project.dynamic` guard: only the literal
    /// `"version"` entry hands version ownership to the build backend, so a
    /// `dynamic` array listing anything else must still be bumped normally.
    #[tokio::test]
    async fn test_write_pyproject_version_allows_dynamic_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            "[project]\nname = \"demo\"\nversion = \"1.0.0\"\ndynamic = [\"readme\"]\n",
        )
        .unwrap();

        write_pyproject_version(&pyproject_toml, "1.1.0")
            .await
            .expect("dynamic without a version entry must still be writable");

        assert_eq!(
            fs::read_to_string(&pyproject_toml).unwrap(),
            "[project]\nname = \"demo\"\nversion = \"1.1.0\"\ndynamic = [\"readme\"]\n"
        );
    }

    /// Renders a realistically formatted `pyproject.toml` at `version`.
    ///
    /// Every construct here is one a re-serializing TOML writer silently
    /// normalizes away: a header comment, `[build-system]` declared BEFORE
    /// `[project]` (non-alphabetical, non-canonical table order), an
    /// end-of-line comment on the version line, a multi-line array with a
    /// trailing comma and an inline table with custom spacing.
    fn round_trip_manifest(version: &str) -> String {
        format!(
            concat!(
                "# demo package manifest - this header comment must survive a bump\n",
                "\n",
                "[build-system]\n",
                "requires = [\"hatchling>=1.18\"]\n",
                "build-backend = \"hatchling.build\"\n",
                "\n",
                "[project]\n",
                "name = \"demo\"\n",
                "version = \"{version}\" # bumped by changepacks\n",
                "dependencies = [\n",
                "    \"httpx>=0.27\",\n",
                "    \"rich>=13\",\n",
                "]\n",
                "\n",
                "[tool.uv.sources]\n",
                "demo-core = {{ path = \"../core\", editable = true }}\n",
            ),
            version = version
        )
    }

    /// Format preservation is a hard project constraint, but until now only
    /// trailing whitespace was pinned. This asserts COMPLETE-FILE equality
    /// (not a `contains` check) so any reformatting `toml_edit` performs -
    /// dropped comment, reordered table, collapsed array or inline table -
    /// fails the test rather than silently rewriting a user's manifest.
    #[tokio::test]
    async fn test_write_pyproject_version_preserves_comments_and_table_order() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(&pyproject_toml, round_trip_manifest("1.2.3")).unwrap();

        write_pyproject_version(&pyproject_toml, "2.0.0")
            .await
            .expect("a well-formed manifest must be writable");

        assert_eq!(
            fs::read_to_string(&pyproject_toml).unwrap(),
            round_trip_manifest("2.0.0"),
            "only the version literal may change; everything else must be byte-identical"
        );
    }
}
