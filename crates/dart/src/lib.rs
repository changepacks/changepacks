//! # changepacks-dart
//!
//! Dart project support for changepacks.
//!
//! Implements project discovery and version management for pubspec.yaml files. Parses YAML
//! with `yaml_serde` and applies format-preserving edits via `yamlpatch`/`yamlpath`. Supports
//! both single packages and workspace configurations with pub as the package manager.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::DartProjectFinder;

/// Default publish command for Dart projects. Shared by `DartPackage`
/// and `DartWorkspace` so a single edit here updates both trait impls.
pub(crate) const PUBLISH_COMMAND: &str = "dart pub publish";

/// Default dry-run publish command for Dart projects.
/// `dart pub publish --dry-run` performs the full pre-flight publish
/// validation (analysis, deps check, LICENSE/CHANGELOG detection) without
/// uploading. Users can override via `publishDryRun` in config.
pub(crate) const DRY_RUN_PUBLISH_COMMAND: &str = "dart pub publish --dry-run";

use std::path::Path;

use anyhow::{Context, Result};
use changepacks_core::UpdateType;
use changepacks_utils::{read_and_parse, write_finalized};

/// Bump `version` by `update_type` and write the result into the `pubspec.yaml`
/// at `path`, preserving the file's YAML formatting.
///
/// Shared by `DartPackage::update_version` and `DartWorkspace::update_version`,
/// whose bodies were byte-identical. The `existing_version` flag MUST be read
/// before `bump_version_with` runs: that call mutates `version` to `Some(..)`
/// unconditionally, while `write_pubspec_version` needs to know whether the
/// manifest already carried a `version` key (replace) or not (add at the
/// document root).
///
/// # Errors
/// Returns an error when semver calculation fails, or when the manifest cannot
/// be read, parsed or written.
pub(crate) async fn bump_pubspec_version(
    version: &mut Option<String>,
    path: &Path,
    update_type: UpdateType,
) -> Result<()> {
    let existing_version = version.is_some();
    changepacks_utils::bump_version_with(version, path, update_type, async |new| {
        write_pubspec_version(path, new, existing_version).await
    })
    .await
}

/// Update `pubspec.yaml` at `path` to set its `version` field to `new_version`,
/// preserving the file's YAML formatting (via `yamlpatch`/`yamlpath`) and its
/// complete trailing-whitespace shape (via `write_finalized`).
///
/// Shared by `DartPackage::update_version` and `DartWorkspace::update_version`
/// so both paths emit byte-identical output. When `existing_version` is
/// `true`, replaces the existing `version` field; when `false`, adds a new
/// `version` field at the document root (needed for workspace roots that
/// declare no `version`).
///
/// # Errors
/// Returns error if the file cannot be read, is not valid YAML, or the write
/// fails.
pub(crate) async fn write_pubspec_version(
    path: &Path,
    new_version: &str,
    existing_version: bool,
) -> Result<()> {
    // The read + parse head is `changepacks_utils::read_and_parse`, the mirror
    // of `write_finalized` below, so the `Failed to read pubspec.yaml <path>`
    // and `Failed to parse pubspec.yaml <path>` contexts are attached in one
    // place shared with every other language crate.
    let (pubspec_yaml_raw, document) =
        read_and_parse(path, "pubspec.yaml", |raw| yamlpath::Document::new(raw)).await?;
    let patch = if existing_version {
        yamlpatch::Patch {
            operation: yamlpatch::Op::Replace(yaml_serde::Value::String(new_version.to_string())),
            route: yamlpath::route!("version"),
        }
    } else {
        yamlpatch::Patch {
            operation: yamlpatch::Op::Add {
                key: "version".to_string(),
                value: yaml_serde::Value::String(new_version.to_string()),
            },
            route: yamlpath::route!(),
        }
    };
    let patched = yamlpatch::apply_yaml_patches(&document, &[patch])
        .with_context(|| format!("Failed to update pubspec.yaml {}", path.display()))?;
    write_finalized(
        path,
        patched.source().to_string(),
        &pubspec_yaml_raw,
        "pubspec.yaml",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_pubspec_version_preserves_complete_trailing_whitespace() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_yaml = temp_dir.path().join("pubspec.yaml");
        let suffix = " \r\n  \n";
        fs::write(&pubspec_yaml, format!("name: test\nversion: 1.0.0{suffix}")).unwrap();

        write_pubspec_version(&pubspec_yaml, "2.0.0", true)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&pubspec_yaml).unwrap(),
            format!("name: test\nversion: 2.0.0{suffix}")
        );
    }

    /// The `existing_version == false` branch (`yamlpatch::Op::Add` at the
    /// document root) runs through the same `write_finalized` tail as the
    /// replace branch, but only ever had coverage over plain single-newline
    /// files. Pin the tail here too: a manifest with no `version` key whose
    /// bytes end in a mixed carriage-return / space suffix must come back with
    /// that suffix byte-identical, not normalized to `\n`.
    #[tokio::test]
    async fn test_write_pubspec_version_add_branch_preserves_complete_trailing_whitespace() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_yaml = temp_dir.path().join("pubspec.yaml");
        let suffix = " \r\n  \n";
        fs::write(
            &pubspec_yaml,
            format!("name: my_workspace\ndescription: A workspace root.{suffix}"),
        )
        .unwrap();

        write_pubspec_version(&pubspec_yaml, "0.1.0", false)
            .await
            .unwrap();

        // `yamlpatch` keeps the last content line's own ` \r\n` ending and
        // appends the new key after it; `write_finalized` then restores the
        // manifest's complete ` \r\n  \n` suffix at the very end.
        assert_eq!(
            fs::read_to_string(&pubspec_yaml).unwrap(),
            format!(
                "name: my_workspace\ndescription: A workspace root. \r\nversion: 0.1.0{suffix}"
            )
        );
    }

    /// A realistic `pubspec.yaml` exercising every formatting feature the
    /// `yamlpatch` round-trip has to survive: a leading comment, an
    /// interleaved comment, blank lines, a quoted scalar, a nested
    /// `dependencies` map containing a `path` entry, and a `dev_dependencies`
    /// block.
    const PUBSPEC_WITH_VERSION: &str = concat!(
        "# Widget package manifest.\n",
        "name: widget\n",
        "description: A widget package.\n",
        "version: 1.2.3\n",
        "\n",
        "# The SDK constraint must survive the rewrite untouched.\n",
        "environment:\n",
        "  sdk: '>=3.0.0 <4.0.0'\n",
        "\n",
        "dependencies:\n",
        "  collection: ^1.18.0\n",
        "  local_helper:\n",
        "    path: ../local_helper\n",
        "\n",
        "dev_dependencies:\n",
        "  test: ^1.24.0\n",
    );

    /// Replacing an existing `version` must rewrite ONLY the version literal:
    /// every comment, blank line, quoting style and nesting level is compared
    /// with full-file equality, so any formatting damage fails the test.
    #[tokio::test]
    async fn test_write_pubspec_version_round_trip_preserves_comments_and_layout() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_yaml = temp_dir.path().join("pubspec.yaml");
        fs::write(&pubspec_yaml, PUBSPEC_WITH_VERSION).unwrap();

        write_pubspec_version(&pubspec_yaml, "2.0.0", true)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&pubspec_yaml).unwrap(),
            concat!(
                "# Widget package manifest.\n",
                "name: widget\n",
                "description: A widget package.\n",
                "version: 2.0.0\n",
                "\n",
                "# The SDK constraint must survive the rewrite untouched.\n",
                "environment:\n",
                "  sdk: '>=3.0.0 <4.0.0'\n",
                "\n",
                "dependencies:\n",
                "  collection: ^1.18.0\n",
                "  local_helper:\n",
                "    path: ../local_helper\n",
                "\n",
                "dev_dependencies:\n",
                "  test: ^1.24.0\n",
            )
        );
    }

    /// The `existing_version == false` branch routes through `yamlpatch::Op::Add`
    /// at the document root (workspace roots that declare no `version`). The
    /// key must be inserted while every pre-existing line and comment survives
    /// byte-for-byte, so this too asserts on the whole file.
    #[tokio::test]
    async fn test_write_pubspec_version_add_branch_preserves_existing_lines() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_yaml = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_yaml,
            concat!(
                "# Workspace root manifest.\n",
                "name: my_workspace\n",
                "\n",
                "environment:\n",
                "  sdk: '>=3.0.0 <4.0.0'\n",
                "\n",
                "# Members of the workspace.\n",
                "workspace:\n",
                "  - packages/alpha\n",
                "  - packages/beta\n",
                "\n",
                "dev_dependencies:\n",
                "  test: ^1.24.0\n",
            ),
        )
        .unwrap();

        write_pubspec_version(&pubspec_yaml, "0.1.0", false)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&pubspec_yaml).unwrap(),
            concat!(
                "# Workspace root manifest.\n",
                "name: my_workspace\n",
                "\n",
                "environment:\n",
                "  sdk: '>=3.0.0 <4.0.0'\n",
                "\n",
                "# Members of the workspace.\n",
                "workspace:\n",
                "  - packages/alpha\n",
                "  - packages/beta\n",
                "\n",
                "dev_dependencies:\n",
                "  test: ^1.24.0\n",
                "version: 0.1.0\n",
            )
        );
    }

    /// When the caller claims an existing `version` key but the manifest has
    /// none, the `Replace` patch route cannot resolve. That failure must be
    /// reported with the `Failed to update pubspec.yaml <path>` context (the
    /// `with_context` on `apply_yaml_patches`), and it must leave the manifest
    /// byte-identical so a rejected patch never produces a partial write.
    #[tokio::test]
    async fn test_write_pubspec_version_unresolvable_replace_route_keeps_file_intact() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_yaml = temp_dir.path().join("pubspec.yaml");
        let original = "name: test\n";
        fs::write(&pubspec_yaml, original).unwrap();

        // `existing_version = true` forces the Replace branch, but there is no
        // `version` key for `yamlpath::route!("version")` to point at.
        let result = write_pubspec_version(&pubspec_yaml, "2.0.0", true).await;

        let err = result.expect_err("replacing a missing version key must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to update pubspec.yaml {}",
                pubspec_yaml.display()
            )),
            "error chain should carry the update context with the path, got: {chain}"
        );
        assert_eq!(
            fs::read_to_string(&pubspec_yaml).unwrap(),
            original,
            "a failed patch must not partially write the manifest"
        );
    }

    /// The read + parse head of `write_pubspec_version` is
    /// `read_and_parse(path, "pubspec.yaml", ..)`. A manifest that `yamlpath`
    /// cannot parse must therefore surface the
    /// `Failed to parse pubspec.yaml <path>` context — pinning the label this
    /// crate passes — and, because the failure happens strictly before any
    /// write, must leave the file byte-identical.
    #[tokio::test]
    async fn test_write_pubspec_version_parse_error_includes_path_and_leaves_file_intact() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_yaml = temp_dir.path().join("pubspec.yaml");
        // An unclosed flow sequence: valid-looking YAML that never terminates.
        let original = "name: test\nversion: [1.0.0\n";
        fs::write(&pubspec_yaml, original).unwrap();

        let result = write_pubspec_version(&pubspec_yaml, "2.0.0", true).await;

        let err = result.expect_err("malformed YAML must fail the parse");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to parse pubspec.yaml {}",
                pubspec_yaml.display()
            )),
            "error chain should carry the parse label and path context, got: {chain}"
        );
        assert_eq!(
            fs::read_to_string(&pubspec_yaml).unwrap(),
            original,
            "a failed parse must not write the manifest at all"
        );
    }

    #[tokio::test]
    async fn test_write_pubspec_version_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_yaml = temp_dir.path().join("pubspec.yaml");
        fs::write(&pubspec_yaml, "name: test\nversion: 1.0.0\n").unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        test_support::set_readonly(&pubspec_yaml, true);

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result = write_pubspec_version(&pubspec_yaml, "2.0.0", true).await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        test_support::set_readonly(&pubspec_yaml, false);

        let err = result.expect_err("write to a readonly pubspec.yaml must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&pubspec_yaml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }
}
