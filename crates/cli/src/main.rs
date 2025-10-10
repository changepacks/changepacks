use std::error::Error;

use clap::{Parser, Subcommand};

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
