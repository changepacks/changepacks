use std::fs::{create_dir_all, write};

use anyhow::Result;
use clap::Args;
use utils::find_current_git_repo::find_current_git_repo;

#[derive(Args, Debug)]
#[command(about = "Initialize a new Changepack project")]
pub struct InitArgs {}

/// Initialize a new Changepack project
pub fn handle_init(args: &InitArgs) -> Result<()> {
    let repo = find_current_git_repo()?;
    // create .changepack directory
    let changepack_dir = repo.workdir().unwrap().join(".changepack");
    create_dir_all(&changepack_dir)?;
    // create changepack.json file
    let changepack_file = changepack_dir.join("changepack.json");
    if changepack_file.exists() {
        Err(anyhow::anyhow!("Changepack project already initialized"))
    } else {
        write(changepack_file, "{}")?;

        println!(
            "Changepack project initialized in {}",
            changepack_dir.display()
        );

        Ok(())
    }
}
