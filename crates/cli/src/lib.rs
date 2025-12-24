use anyhow::Result;

use changepacks_core::UpdateType;
use clap::{Parser, Subcommand, ValueEnum};

use crate::{
    commands::{
        ChangepackArgs, CheckArgs, ConfigArgs, InitArgs, PublishArgs, UpdateArgs,
        handle_changepack, handle_check, handle_config, handle_init, handle_publish, handle_update,
    },
    options::FilterOptions,
};
mod commands;
mod finders;
mod options;

#[derive(ValueEnum, Debug, Clone)]
enum CliUpdateType {
    Major,
    Minor,
    Patch,
}

impl From<CliUpdateType> for UpdateType {
    fn from(value: CliUpdateType) -> Self {
        match value {
            CliUpdateType::Major => UpdateType::Major,
            CliUpdateType::Minor => UpdateType::Minor,
            CliUpdateType::Patch => UpdateType::Patch,
        }
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about = "changepacks CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long)]
    filter: Option<FilterOptions>,

    #[arg(short, long, default_value = "false")]
    remote: bool,

    #[arg(short, long, default_value = "false")]
    yes: bool,

    #[arg(short, long)]
    message: Option<String>,

    #[arg(short, long)]
    update_type: Option<CliUpdateType>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init(InitArgs),
    Check(CheckArgs),
    Update(UpdateArgs),
    Config(ConfigArgs),
    Publish(PublishArgs),
}

pub async fn main(args: &[String]) -> Result<()> {
    let cli = Cli::parse_from(args);
    if let Some(command) = cli.command {
        match command {
            Commands::Init(args) => handle_init(&args).await?,
            Commands::Check(args) => handle_check(&args).await?,
            Commands::Update(args) => handle_update(&args).await?,
            Commands::Config(args) => handle_config(&args).await?,
            Commands::Publish(args) => handle_publish(&args).await?,
        }
    } else {
        handle_changepack(&ChangepackArgs {
            filter: cli.filter,
            remote: cli.remote,
            yes: cli.yes,
            message: cli.message,
            update_type: cli.update_type.map(Into::into),
        })
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(CliUpdateType::Major, UpdateType::Major)]
    #[case(CliUpdateType::Minor, UpdateType::Minor)]
    #[case(CliUpdateType::Patch, UpdateType::Patch)]
    fn test_cli_update_type_to_update_type(
        #[case] cli_type: CliUpdateType,
        #[case] expected: UpdateType,
    ) {
        let result: UpdateType = cli_type.into();
        assert_eq!(result, expected);
    }
}
