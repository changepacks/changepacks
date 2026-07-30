use anyhow::{Context, Result};
use changepacks_utils::get_changepacks_config;
use std::io::Write;

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
    writeln!(out, "{}", serde_json::to_string_pretty(&config)?)?;
    Ok(())
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
}
