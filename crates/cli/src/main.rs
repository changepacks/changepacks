use core::proejct_finder::ProjectFinder;
use std::{error::Error, fs};
use update::UpdateArgs;

use clap::{Parser, Subcommand};
use node::NodeProjectFinder;
use python::PythonProjectFinder;
use rust::RustProjectFinder;
use utils::{filter_project_dirs::find_project_dirs, find_current_git_repo::find_current_git_repo};

use crate::changepack::handle_changepack;
mod changepack;
mod check;
mod init;
mod update;

#[derive(Parser, Debug)]
#[command(author, version, about = "Changepack CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}
#[derive(Subcommand, Debug)]
enum Commands {
    Init(init::InitArgs),
    Check(check::CheckArgs),
    Update(update::UpdateArgs),
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    if let Some(command) = cli.command {
        match command {
            Commands::Init(args) => init::handle_init(&args)?,
            Commands::Check(args) => check::handle_check(&args)?,
            Commands::Update(args) => update::handle_update(&args)?,
        }
    } else {
        handle_changepack()?;
    }
    Ok(())
}
