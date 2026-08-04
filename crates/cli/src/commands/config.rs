use anyhow::{Context, Result};
use changepacks_core::Config;
use changepacks_utils::get_changepacks_config;
use std::io::Write;

/// Renders `config` as pretty-printed JSON into `writer`, followed by a newline.
///
/// Split out of [`handle_config`] so the emitted bytes — the `.changepacks/config.json`
/// shape that `changepacks config` promises to keep backward compatible — can be
/// asserted against an in-memory buffer instead of the process stdout. Mirrors
/// `FormatOptions::write_message`.
///
/// # Errors
/// Returns an error if serialization fails or `writer` reports an io error.
fn write_config<W: Write>(writer: &mut W, config: &Config) -> Result<()> {
    writeln!(writer, "{}", serde_json::to_string_pretty(config)?)?;
    Ok(())
}

/// Display changepacks configuration
///
/// # Errors
/// Returns error if reading the configuration fails.
pub async fn handle_config() -> Result<()> {
    let current_dir =
        std::env::current_dir().context("Failed to determine current working directory")?;
    // `get_changepacks_config` only contextualizes its read/parse failures; the
    // repository-discovery step it performs first propagates bare, so without
    // this the `changepacks config` user sees a raw gix error with no hint that
    // it came from loading the configuration.
    let config = get_changepacks_config(&current_dir)
        .await
        .context("Failed to load changepacks configuration")?;
    // One stdout lock for the whole render: `println!` re-acquires the global
    // lock per line and panics on a write failure (a broken pipe from
    // `changepacks config | head`), while a held `StdoutLock` writes through the
    // same `LineWriter` and lets an io error propagate as a typed error.
    let mut out = std::io::stdout().lock();
    write_config(&mut out, &config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support::DirGuard;
    use serial_test::serial;
    use tempfile::TempDir;

    /// Outside a git repository the config load fails in repository discovery,
    /// which is the one failure path `get_changepacks_config` does not
    /// contextualize itself. Pin the command-level context so the user always
    /// learns which step failed.
    #[tokio::test]
    #[serial]
    async fn test_handle_config_outside_git_repo_reports_load_context() {
        let temp_dir = TempDir::new().expect("create temporary directory");
        let _dir_guard = DirGuard::change_to(temp_dir.path());

        let err = handle_config()
            .await
            .expect_err("config load must fail outside a git repository");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("Failed to load changepacks configuration"),
            "unexpected error: {rendered}"
        );
    }

    /// The bytes `changepacks config` prints for a default config are a
    /// backward-compatibility contract: every camelCase key documented in the
    /// README sample must be present, `latestPackage` must render as `null`
    /// rather than being omitted, and the render must end with exactly one
    /// trailing newline.
    #[test]
    fn test_write_config_renders_default_config() {
        let mut buffer = Vec::new();
        write_config(&mut buffer, &Config::default()).expect("write default config");

        let rendered = String::from_utf8(buffer).expect("config render must be utf-8");
        assert_eq!(
            rendered,
            concat!(
                "{\n",
                "  \"ignore\": [],\n",
                "  \"baseBranch\": \"main\",\n",
                "  \"latestPackage\": null,\n",
                "  \"publish\": {},\n",
                "  \"publishDryRun\": {},\n",
                "  \"updateOn\": {}\n",
                "}\n"
            )
        );
        assert!(rendered.ends_with("}\n") && !rendered.ends_with("}\n\n"));
    }

    /// A fully populated config pins the camelCase key spellings and the
    /// nested indentation of the maps and arrays, so a rename or a serde
    /// attribute change on `Config` cannot silently break existing
    /// `.changepacks/config.json` consumers.
    #[test]
    fn test_write_config_renders_populated_config() {
        let mut config = Config {
            ignore: vec![
                "**/*".to_string(),
                "!crates/changepacks/Cargo.toml".to_string(),
            ],
            base_branch: "main".to_string(),
            latest_package: Some("crates/changepacks/Cargo.toml".to_string()),
            ..Config::default()
        };
        config
            .publish
            .insert("rust".to_string(), "cargo publish".to_string());
        config.publish.insert(
            "bridge/node/package.json".to_string(),
            "npm publish --access public".to_string(),
        );
        config
            .publish_dry_run
            .insert("csharp".to_string(), "dotnet pack -c Release".to_string());
        config.update_on.insert(
            "crates/changepacks/Cargo.toml".to_string(),
            vec![
                "bridge/node/package.json".to_string(),
                "bridge/python/pyproject.toml".to_string(),
            ],
        );

        let mut buffer = Vec::new();
        write_config(&mut buffer, &config).expect("write populated config");

        let rendered = String::from_utf8(buffer).expect("config render must be utf-8");
        assert_eq!(
            rendered,
            concat!(
                "{\n",
                "  \"ignore\": [\n",
                "    \"**/*\",\n",
                "    \"!crates/changepacks/Cargo.toml\"\n",
                "  ],\n",
                "  \"baseBranch\": \"main\",\n",
                "  \"latestPackage\": \"crates/changepacks/Cargo.toml\",\n",
                "  \"publish\": {\n",
                "    \"bridge/node/package.json\": \"npm publish --access public\",\n",
                "    \"rust\": \"cargo publish\"\n",
                "  },\n",
                "  \"publishDryRun\": {\n",
                "    \"csharp\": \"dotnet pack -c Release\"\n",
                "  },\n",
                "  \"updateOn\": {\n",
                "    \"crates/changepacks/Cargo.toml\": [\n",
                "      \"bridge/node/package.json\",\n",
                "      \"bridge/python/pyproject.toml\"\n",
                "    ]\n",
                "  }\n",
                "}\n"
            )
        );
    }

    /// A write failure mid-render (a `BrokenPipe` from `changepacks config | head`)
    /// must surface as a typed error instead of panicking, which is the whole
    /// reason `handle_config` holds a `StdoutLock` rather than using `println!`.
    #[test]
    fn test_write_config_propagates_write_error() {
        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pipe closed",
                ))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let error = write_config(&mut FailingWriter, &Config::default())
            .expect_err("write error must propagate");
        let io_error = error
            .downcast_ref::<std::io::Error>()
            .expect("write failure must stay an io::Error");
        assert_eq!(io_error.kind(), std::io::ErrorKind::BrokenPipe);
    }
}
