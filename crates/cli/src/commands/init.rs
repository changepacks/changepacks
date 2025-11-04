use changepacks_core::Config;
use tokio::fs::{create_dir_all, write};

use anyhow::Result;
use clap::Args;
use utils::get_changepacks_dir;

#[derive(Args, Debug)]
#[command(about = "Initialize a new changepacks project")]
pub struct InitArgs {
    /// If true, do not make any filesystem changes.
    #[arg(short, long, default_value = "false")]
    dry_run: bool,
}

/// Initialize a new changepacks project
pub async fn handle_init(args: &InitArgs) -> Result<()> {
    // create .changepacks directory
    let current_dir = std::env::current_dir()?;
    let changepacks_dir = get_changepacks_dir(&current_dir)?;
    if !args.dry_run {
        create_dir_all(&changepacks_dir).await?;
    }
    // create config.json file
    let config_file = changepacks_dir.join("config.json");
    if config_file.exists() {
        Err(anyhow::anyhow!("changepacks project already initialized"))
    } else {
        if !args.dry_run {
            write(
                config_file,
                serde_json::to_string_pretty(&Config::default())?,
            )
            .await?;
        }

        println!(
            "changepacks project initialized in {}",
            changepacks_dir.display()
        );

        Ok(())
    }
}
