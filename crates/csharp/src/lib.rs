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
mod xml_utils;

use std::path::Path;

use anyhow::{Context, Result};
use tokio::fs::{read_to_string, write};

pub use finder::CSharpProjectFinder;

/// Legacy command description required by the core `Package` / `Workspace`
/// trait API. C# publish execution does not run this incomplete shell string:
/// both implementations override `publish` and use the managed argv pipeline
/// in [`dry_run`] after resolving configuration overrides. Keeping the string
/// preserves the existing public accessor value for callers that display it.
pub(crate) const PUBLISH_COMMAND: &str = "dotnet pack -c Release && dotnet nuget push";

/// Read the `.csproj` file at `path` into a `String`, attaching the shared
/// read-failure context naming that path on failure.
///
/// Single source of the read + error-context pair used by both `.csproj`
/// entry points: [`write_csproj_version`] here and
/// [`CSharpProjectFinder::visit`](finder::CSharpProjectFinder) during
/// discovery. Keeping one copy keeps the two call sites' error messages
/// from drifting apart.
///
/// # Errors
/// Returns an error if the file cannot be read (missing, unreadable, or not
/// valid UTF-8).
pub(crate) async fn read_csproj(path: &Path) -> Result<String> {
    read_to_string(path)
        .await
        .with_context(|| format!("Failed to read C# project {}", path.display()))
}

/// Update the `<Version>` element of the `.csproj` XML at `path` to
/// `new_version`, delegating to [`xml_utils::update_version_in_xml`] to
/// preserve the file's original formatting (indentation, comments, sibling
/// elements). The XML is re-scanned at write time so missing global versions
/// are added under the first unconditional top-level `<PropertyGroup>`, or
/// under a newly created group when none is eligible (see `update_version_in_xml`).
///
/// Used by `CSharpPackage::update_version`, the only project kind
/// [`CSharpProjectFinder`] discovers — matching the Node/Python/Dart
/// convention documented in `crates/AGENTS.md`.
///
/// # Errors
/// Returns error if the file cannot be read, the XML cannot be parsed, or
/// no supported version node can be mutated, or the write fails.
pub(crate) async fn write_csproj_version(path: &Path, new_version: &str) -> Result<()> {
    let csproj_raw = read_csproj(path).await?;
    let updated = xml_utils::update_version_in_xml(&csproj_raw, new_version)
        .with_context(|| format!("Failed to update version in C# project {}", path.display()))?;
    if updated != csproj_raw {
        write(path, updated)
            .await
            .with_context(|| format!("Failed to write C# project {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support;
    use tempfile::TempDir;

    /// A realistic `.csproj` shared by the two round-trip tests below: CRLF
    /// line endings, an XML declaration, a comment, two-space indentation, an
    /// existing `<Version>1.0.0</Version>` with sibling properties on both
    /// sides, and a trailing blank line. Every one of those is formatting
    /// `write_csproj_version` must carry through untouched.
    const REALISTIC_CSPROJ_CRLF: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<Project Sdk=\"Microsoft.NET.Sdk\">\r\n  <!-- Package metadata -->\r\n  <PropertyGroup>\r\n    <TargetFramework>net8.0</TargetFramework>\r\n    <Version>1.0.0</Version>\r\n    <Nullable>enable</Nullable>\r\n  </PropertyGroup>\r\n</Project>\r\n\r\n";

    /// `write_csproj_version` is the only manifest writer that does not route
    /// through `finalize_content`; its format preservation rests entirely on
    /// `xml_utils::update_version_in_xml` round-tripping everything but the
    /// version text. Lock that end to end at the file boundary: the bytes on
    /// disk after the bump must equal the input with ONLY `1.0.0` -> `1.0.1`.
    #[tokio::test]
    async fn test_write_csproj_version_preserves_surrounding_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Formatted.csproj");
        tokio::fs::write(&csproj_path, REALISTIC_CSPROJ_CRLF)
            .await
            .unwrap();

        write_csproj_version(&csproj_path, "1.0.1").await.unwrap();

        let expected =
            REALISTIC_CSPROJ_CRLF.replace("<Version>1.0.0</Version>", "<Version>1.0.1</Version>");
        assert_eq!(
            tokio::fs::read_to_string(&csproj_path).await.unwrap(),
            expected,
            "only the version text may change; every other byte must survive",
        );
        temp_dir.close().unwrap();
    }

    /// The `if updated != csproj_raw` guard in `write_csproj_version` must
    /// skip the write entirely when the requested version already matches, so
    /// a no-op `changepacks update` never rewrites (and never risks
    /// reformatting or touching the mtime of) an unchanged `.csproj`.
    #[tokio::test]
    async fn test_write_csproj_version_skips_write_when_version_unchanged() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Unchanged.csproj");
        tokio::fs::write(&csproj_path, REALISTIC_CSPROJ_CRLF)
            .await
            .unwrap();
        let modified_before = tokio::fs::metadata(&csproj_path)
            .await
            .unwrap()
            .modified()
            .unwrap();

        write_csproj_version(&csproj_path, "1.0.0").await.unwrap();

        assert_eq!(
            tokio::fs::read(&csproj_path).await.unwrap(),
            REALISTIC_CSPROJ_CRLF.as_bytes(),
            "an unchanged version must leave the file byte-identical",
        );
        // A skipped write cannot move the mtime. (The converse is weaker --
        // a coarse filesystem clock could hide a real write -- so this only
        // ever strengthens the byte assertion above, never contradicts it.)
        assert_eq!(
            tokio::fs::metadata(&csproj_path)
                .await
                .unwrap()
                .modified()
                .unwrap(),
            modified_before,
            "the write-skip guard must not touch the file at all",
        );
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_write_csproj_version_creates_property_group_when_missing() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("NoPropertyGroup.csproj");
        let content = b"<Project Sdk=\"Microsoft.NET.Sdk\">\r\n</Project>\r\n";
        tokio::fs::write(&csproj_path, content).await.unwrap();

        write_csproj_version(&csproj_path, "1.2.3").await.unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&csproj_path).await.unwrap(),
            "<Project Sdk=\"Microsoft.NET.Sdk\">\r\n<PropertyGroup>\r\n    <Version>1.2.3</Version>\r\n</PropertyGroup>\r\n</Project>\r\n"
        );
        temp_dir.close().unwrap();
    }

    /// The read leg of `write_csproj_version` (via [`read_csproj`]) must name
    /// the manifest path in its error chain, so a missing/unreadable `.csproj`
    /// is diagnosable from the CLI output alone. Pins the exact
    /// `Failed to read C# project {path}` context added at the `read_csproj`
    /// call site — same message-pinning style as the Node/utils siblings.
    #[tokio::test]
    async fn test_write_csproj_version_read_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Missing.csproj");

        let err = write_csproj_version(&csproj_path, "1.0.1")
            .await
            .expect_err("a missing .csproj must fail the read");

        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to read C# project {}",
                csproj_path.display()
            )),
            "error chain should carry the read context naming the manifest path, got: {chain}"
        );
        temp_dir.close().unwrap();
    }

    /// The update leg of `write_csproj_version` must name the manifest path in
    /// its error chain: `update_version_in_xml` only reports the XML-level
    /// cause (`XML parsing error: ...`), so without the `.with_context(...)`
    /// wrapper a user with many `.csproj` files could not tell WHICH file is
    /// malformed. Uses an unbalanced `<Project>`/`<PropertyGroup>` document so
    /// the XML parse — not the read — is what fails.
    #[tokio::test]
    async fn test_write_csproj_version_update_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Malformed.csproj");
        tokio::fs::write(
            &csproj_path,
            "<Project><PropertyGroup><Version>1.0.0</Version></PropertyGroup",
        )
        .await
        .unwrap();

        let err = write_csproj_version(&csproj_path, "1.0.1")
            .await
            .expect_err("a malformed .csproj must fail the version update");

        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to update version in C# project {}",
                csproj_path.display()
            )),
            "error chain should carry the update context naming the manifest path, got: {chain}"
        );
        temp_dir.close().unwrap();
    }

    /// The write leg is the last of the three `write_csproj_version` error
    /// contexts and the only one left unpinned: `tokio::fs::write` reports a
    /// bare `os error` (permission denied) with no filename, so without the
    /// `.with_context(...)` wrapper a failed bump would be unattributable to a
    /// particular `.csproj`. Mirrors the Java sibling
    /// (`version_updater.rs::test_write_gradle_version_build_file_write_error_names_context_and_path`):
    /// seed a valid manifest, flip the readonly bit AFTER seeding so the read
    /// and the XML update both still succeed, and request a DIFFERENT version
    /// so the `updated != csproj_raw` guard actually attempts the write.
    #[tokio::test]
    async fn test_write_csproj_version_write_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Readonly.csproj");
        tokio::fs::write(&csproj_path, REALISTIC_CSPROJ_CRLF)
            .await
            .unwrap();

        test_support::set_readonly(&csproj_path, true);

        let result = write_csproj_version(&csproj_path, "1.0.1").await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion below panics.
        test_support::set_readonly(&csproj_path, false);

        let err = result.expect_err("a write to a readonly .csproj must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to write C# project {}",
                csproj_path.display()
            )),
            "error chain should carry the write context naming the manifest path, got: {chain}"
        );
        temp_dir.close().unwrap();
    }
}
