use anyhow::{Context, Result};
use changepacks_utils::get_changepacks_config;
use clap::Args;
use std::io::Write;

#[derive(Args, Debug)]
#[command(about = "Change changepacks configuration")]
pub struct ConfigArgs {}

/// Display changepacks configuration
///
/// # Errors
/// Returns error if reading the configuration fails.
pub async fn handle_config() -> Result<()> {
    let current_dir =
        std::env::current_dir().context("Failed to determine current working directory")?;
    let config = get_changepacks_config(&current_dir).await?;
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
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        config: ConfigArgs,
    }

    #[test]
    fn test_config_args_parsing() {
        // ConfigArgs has no arguments, just verify it parses
        let _cli = TestCli::parse_from(["test"]);
    }

    #[test]
    fn test_config_args_debug() {
        let args = ConfigArgs {};
        let debug_str = format!("{args:?}");
        assert!(debug_str.contains("ConfigArgs"));
    }
}
