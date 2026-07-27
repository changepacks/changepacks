use std::path::Path;

use anyhow::{Context, Result};

/// Return the complete trailing-whitespace suffix of `source`.
#[must_use]
fn trailing_whitespace(source: &str) -> &str {
    &source[source.trim_end().len()..]
}

/// Assemble the finalized on-disk bytes for a manifest rewrite by dropping
/// only the serializer-generated trailing whitespace from `body`, then
/// re-appending the original source's complete trailing-whitespace suffix.
/// The suffix starts immediately after the original's final non-whitespace
/// character and is therefore preserved byte-for-byte, including repeated or
/// mixed line endings, spaces, tabs, and other Unicode whitespace.
#[must_use]
pub fn finalize_content(mut body: String, original: &str) -> String {
    let trimmed_len = body.trim_end().len();
    body.truncate(trimmed_len);
    body.push_str(trailing_whitespace(original));
    body
}

/// Write the finalized manifest bytes for `path`: run `body` through
/// [`finalize_content`] against the manifest's `original` on-disk text, then
/// write the result, attaching a `Failed to write <label> <path>` context to
/// any I/O failure.
///
/// This is the shared tail of every language crate's manifest rewrite
/// (`package.json`, `pyproject.toml`, `pubspec.yaml`, `Cargo.toml`), which
/// previously open-coded the identical `write(path, finalize_content(..))
/// .await.with_context(..)` sequence at six call sites. `label` is the
/// human-facing manifest name that appears in the error context, and is the
/// only thing that ever differed between them.
///
/// # Errors
/// Returns an error if the write fails. The error context names both the
/// manifest kind (`label`) and the path, so a failed write reads as clearly
/// as the read/parse contexts each caller already attaches.
pub async fn write_finalized(path: &Path, body: String, original: &str, label: &str) -> Result<()> {
    tokio::fs::write(path, finalize_content(body, original))
        .await
        .with_context(|| format!("Failed to write {label} {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // Empty / no trailing whitespace.
    #[case("", "")]
    #[case("hello", "")]
    #[case("a\nb", "")]
    // Complete whitespace suffixes, including an all-whitespace source.
    #[case("hello\n", "\n")]
    #[case("hello\n\n", "\n\n")]
    #[case("hello\r\n\r\n", "\r\n\r\n")]
    #[case("hello \t\r\n \n", " \t\r\n \n")]
    #[case(" \t\u{000c}\r\n", " \t\u{000c}\r\n")]
    fn test_trailing_whitespace(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(trailing_whitespace(input), expected);
    }

    #[rstest]
    #[case("hello  \n\n", "content\n", "hello\n")]
    #[case("hello  \n", "content\n\n", "hello\n\n")]
    #[case("hello  \n", "content\r\n\r\n", "hello\r\n\r\n")]
    #[case("hello\n", "content \t\r\n \n", "hello \t\r\n \n")]
    #[case("hello\n", "content\u{000c}\t", "hello\u{000c}\t")]
    #[case("hello\t\t", "content", "hello")]
    #[case("hello", " \t\r\n", "hello \t\r\n")]
    fn test_finalize_content(#[case] body: &str, #[case] original: &str, #[case] expected: &str) {
        assert_eq!(finalize_content(body.to_string(), original), expected);
    }

    #[rstest]
    #[case("hello\r\n\r\n", "content\r\n\r\n")]
    #[case("hello\n\n", "content\n\n")]
    #[case("hello \t\r\n \n", "content \t\r\n \n")]
    #[case("hello", "content")]
    fn test_finalize_content_preserves_suffix_on_repeated_calls(
        #[case] expected_body: &str,
        #[case] original: &str,
    ) {
        let once = finalize_content("hello \t\r\n".to_string(), original);
        let twice = finalize_content(once.clone(), &once);

        assert_eq!(once, expected_body);
        assert_eq!(twice, expected_body);
    }

    #[tokio::test]
    async fn test_write_finalized_writes_body_with_original_trailing_whitespace() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manifest = temp_dir.path().join("manifest.toml");
        let original = "version = \"1.0.0\" \t\r\n \n";
        std::fs::write(&manifest, original).unwrap();

        write_finalized(
            &manifest,
            "version = \"2.0.0\"\n".to_string(),
            original,
            "Cargo.toml",
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&manifest).unwrap(),
            "version = \"2.0.0\" \t\r\n \n"
        );
    }

    /// The error context must read `Failed to write <label> <path>` so the six
    /// migrated manifest writers keep emitting byte-identical messages.
    #[tokio::test]
    async fn test_write_finalized_error_context_names_label_and_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manifest = temp_dir.path().join("manifest.json");
        std::fs::write(&manifest, "{}\n").unwrap();

        // Readonly denies the write-open on every supported platform.
        crate::test_support::set_readonly(&manifest, true);
        let result =
            write_finalized(&manifest, "{\"a\":1}\n".to_string(), "{}\n", "package.json").await;
        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        crate::test_support::set_readonly(&manifest, false);

        let err = result.expect_err("write to a readonly manifest must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to write package.json {}",
                manifest.display()
            )),
            "error chain should carry the label and path context, got: {chain}"
        );
    }
}
