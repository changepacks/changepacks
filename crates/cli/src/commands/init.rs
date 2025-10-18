use tokio::fs::{create_dir_all, write};

use anyhow::Result;
use clap::Args;
use utils::get_changepack_dir;

#[derive(Args, Debug)]
#[command(about = "Initialize a new Changepack project")]
pub struct InitArgs {
    /// If true, do not make any filesystem changes.
    dry_run: bool,
}

/// Initialize a new Changepack project
pub async fn handle_init(args: &InitArgs) -> Result<()> {
    // create .changepack directory
    let changepack_dir = get_changepack_dir()?;
    if !args.dry_run {
        create_dir_all(&changepack_dir).await?;
    }
    // create changepack.json file
    let changepack_file = changepack_dir.join("changepack.json");
    if changepack_file.exists() {
        Err(anyhow::anyhow!("Changepack project already initialized"))
    } else {
        if !args.dry_run {
            write(changepack_file, "{}").await?;
        }

        println!(
            "Changepack project initialized in {}",
            changepack_dir.display()
        );

        Ok(())
    }
}
