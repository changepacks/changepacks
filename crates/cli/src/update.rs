use anyhow::Result;
use clap::Args;
use utils::find_current_git_repo;

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct UpdateArgs {
    #[arg(short, long)]
    dry_run: bool,
}

/// Update project version
pub fn handle_update(args: &UpdateArgs) -> Result<()> {
    let repo = find_current_git_repo()?;
    // check if changepack.json exists
    let changepack_file = repo.workdir().unwrap().join(".changepack/changepack.json");
    if !changepack_file.exists() {
        return Err(anyhow::anyhow!("Changepack project not initialized"));
    }
    Ok(())
}
