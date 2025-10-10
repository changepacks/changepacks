
use anyhow::Result;
use clap::Args;
use utils::find_current_git_repo::find_current_git_repo;

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct CheckArgs {}

/// Check project status
pub fn handle_check(args: &CheckArgs) -> Result<()> {
    let repo = find_current_git_repo()?;
    // check if changepack.json exists
    let changepack_file = repo.workdir().unwrap().join(".changepack/changepack.json");
    if !changepack_file.exists() {
        Err(anyhow::anyhow!("Changepack project not initialized"))
    } else {
        println!(
            "Changepack project initialized in {}",
            changepack_file.display()
        );
        // Display Project Tree
        println!("Project Tree:");
        println!("{}", changepack_file.display());
        Ok(())
    }
}
