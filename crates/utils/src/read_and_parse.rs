use std::path::Path;

use anyhow::{Context, Result};

/// Read the manifest at `path` and hand its text to `parse`, returning both
/// the raw on-disk text and the parsed value.
///
/// This is the shared HEAD of every language crate's manifest pipeline and
/// the exact mirror of [`crate::write_finalized`], which is the shared TAIL.
/// Five call sites open-coded the identical `read_to_string(..).await
/// .with_context(..)?` then `parse(..).with_context(..)?` sequence
/// (`package.json`, `pyproject.toml`, `Cargo.toml`, and `pubspec.yaml` twice),
/// differing only in the manifest `label` and the parser invoked. `label` is
/// the human-facing manifest name that appears in both error contexts:
///
/// - read failure -> `Failed to read <label> <path>`
/// - parse failure -> `Failed to parse <label> <path>`
///
/// The parser stays a caller-supplied closure on purpose, so `serde_json`,
/// `toml_edit`, `yaml_serde` and `yamlpath` remain dependencies of the
/// language crates that need them and `changepacks-utils` gains no new
/// dependency for this helper.
///
/// The raw text is returned alongside the parsed value because every caller
/// needs it afterwards — for indent detection (`detect_indent_str`) and for
/// the trailing-whitespace shape that [`crate::write_finalized`] replays.
///
/// # Errors
/// Returns an error if the file cannot be read, or if `parse` rejects its
/// contents. Both errors carry the manifest kind (`label`) and the path.
pub async fn read_and_parse<T, E, F>(path: &Path, label: &str, parse: F) -> Result<(String, T)>
where
    F: FnOnce(&str) -> std::result::Result<T, E>,
    E: std::error::Error + Send + Sync + 'static,
{
    let raw = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read {label} {}", path.display()))?;
    let parsed =
        parse(&raw).with_context(|| format!("Failed to parse {label} {}", path.display()))?;
    Ok((raw, parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_json(raw: &str) -> serde_json::Result<serde_json::Value> {
        serde_json::from_str(raw)
    }

    #[tokio::test]
    async fn test_read_and_parse_returns_raw_and_parsed() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manifest = temp_dir.path().join("package.json");
        let original = "{\n  \"version\": \"1.0.0\"\n}\n";
        std::fs::write(&manifest, original).unwrap();

        let (raw, parsed) = read_and_parse(&manifest, "package.json", parse_json)
            .await
            .unwrap();

        // The raw text is returned byte-for-byte so callers can replay the
        // file's indent and trailing-whitespace shape on write-back.
        assert_eq!(raw, original);
        assert_eq!(parsed["version"], serde_json::json!("1.0.0"));
    }

    /// A missing manifest must surface `Failed to read <label> <path>` so the
    /// migrated call sites keep emitting byte-identical messages.
    #[tokio::test]
    async fn test_read_and_parse_read_error_context_names_label_and_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manifest = temp_dir.path().join("missing").join("Cargo.toml");

        let err = read_and_parse(&manifest, "Cargo.toml", parse_json)
            .await
            .expect_err("a missing manifest must fail the read");

        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!("Failed to read Cargo.toml {}", manifest.display())),
            "error chain should carry the read label and path context, got: {chain}"
        );
    }

    /// A malformed manifest must surface `Failed to parse <label> <path>`.
    #[tokio::test]
    async fn test_read_and_parse_parse_error_context_names_label_and_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manifest = temp_dir.path().join("pyproject.toml");
        std::fs::write(&manifest, "{ not json").unwrap();

        let err = read_and_parse(&manifest, "pyproject.toml", parse_json)
            .await
            .expect_err("a malformed manifest must fail the parse");

        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to parse pyproject.toml {}",
                manifest.display()
            )),
            "error chain should carry the parse label and path context, got: {chain}"
        );
    }

    /// The parser's own error must stay in the chain underneath the added
    /// context, so callers keep the underlying diagnostic (line/column).
    #[tokio::test]
    async fn test_read_and_parse_preserves_parser_error_in_chain() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manifest = temp_dir.path().join("package.json");
        std::fs::write(&manifest, "{ not json").unwrap();

        let err = read_and_parse(&manifest, "package.json", parse_json)
            .await
            .expect_err("a malformed manifest must fail the parse");

        let chain = format!("{err:#}");
        assert!(
            chain.contains("line 1"),
            "error chain should retain the parser's own diagnostic, got: {chain}"
        );
    }
}
