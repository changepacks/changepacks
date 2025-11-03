use anyhow::Result;
use clap::Args;
use utils::get_changepacks_config;

#[derive(Args, Debug)]
#[command(about = "Change changepacks configuration")]
pub struct ConfigArgs {}

/// Update project version
pub async fn handle_config(_args: &ConfigArgs) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let config = get_changepacks_config(&current_dir).await?;
    println!("{}", serde_json::to_string_pretty(&config)?);
    Ok(())
}
