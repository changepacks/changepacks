use anyhow::Result;

use clap::{Parser, Subcommand};

use crate::{
    commands::{
        ChangepackArgs, CheckArgs, ConfigArgs, InitArgs, UpdateArgs, handle_changepack,
        handle_check, handle_config, handle_init, handle_update,
    },
    options::FilterOptions,
};
mod commands;
mod finders;
mod options;

#[derive(Parser, Debug)]
#[command(author, version, about = "changepacks CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long)]
    filter: Option<FilterOptions>,
}
#[derive(Subcommand, Debug)]
enum Commands {
    Init(InitArgs),
    Check(CheckArgs),
    Update(UpdateArgs),
    Config(ConfigArgs),
}

pub async fn main(args: &[String]) -> Result<()> {
    let cli = Cli::parse_from(args);
    if let Some(command) = cli.command {
        match command {
            Commands::Init(args) => handle_init(&args).await?,
            Commands::Check(args) => handle_check(&args).await?,
            Commands::Update(args) => handle_update(&args).await?,
            Commands::Config(args) => handle_config(&args).await?,
        }
    } else {
        handle_changepack(&ChangepackArgs { filter: cli.filter }).await?;
    }
    Ok(())
}
