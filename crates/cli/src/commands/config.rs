use anyhow::Result;
use changepacks_utils::get_changepacks_config;
use clap::Args;

#[derive(Args, Debug)]
#[command(about = "Change changepacks configuration")]
pub struct ConfigArgs {}

/// Display changepacks configuration
///
/// # Errors
/// Returns error if reading the configuration fails.
pub async fn handle_config(_args: &ConfigArgs) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let config = get_changepacks_config(&current_dir).await?;
    println!("{}", serde_json::to_string_pretty(&config)?);
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
        let debug_str = format!("{:?}", args);
        assert!(debug_str.contains("ConfigArgs"));
    }
}
