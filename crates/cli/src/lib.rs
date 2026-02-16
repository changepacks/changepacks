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
pub mod commands;
mod finders;
pub mod options;
pub mod prompter;

pub use prompter::UserCancelled;

#[derive(ValueEnum, Debug, Clone)]
enum CliUpdateType {
    Major,
    Minor,
    Patch,
}

impl From<CliUpdateType> for UpdateType {
    fn from(value: CliUpdateType) -> Self {
        match value {
            CliUpdateType::Major => Self::Major,
            CliUpdateType::Minor => Self::Minor,
            CliUpdateType::Patch => Self::Patch,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "changepacks",
    author,
    version,
    about = "A unified version management and changelog tool for multi-language projects",
    help_template = "{name} {version}\n{about}\n\n{usage-heading} {usage}\n\n{all-args}"
)]
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

    // Test that Cli struct parses correctly
    #[test]
    fn test_cli_parsing_init() {
        use clap::Parser;
        let cli = Cli::parse_from(["changepacks", "init"]);
        assert!(matches!(cli.command, Some(Commands::Init(_))));
    }

    #[test]
    fn test_cli_parsing_check() {
        use clap::Parser;
        let cli = Cli::parse_from(["changepacks", "check"]);
        assert!(matches!(cli.command, Some(Commands::Check(_))));
    }

    #[test]
    fn test_cli_parsing_update() {
        use clap::Parser;
        let cli = Cli::parse_from(["changepacks", "update", "--dry-run"]);
        assert!(matches!(cli.command, Some(Commands::Update(_))));
    }

    #[test]
    fn test_cli_parsing_config() {
        use clap::Parser;
        let cli = Cli::parse_from(["changepacks", "config"]);
        assert!(matches!(cli.command, Some(Commands::Config(_))));
    }

    #[test]
    fn test_cli_parsing_publish() {
        use clap::Parser;
        let cli = Cli::parse_from(["changepacks", "publish", "--dry-run"]);
        assert!(matches!(cli.command, Some(Commands::Publish(_))));
    }

    #[test]
    fn test_cli_parsing_default_with_options() {
        use clap::Parser;
        let cli = Cli::parse_from([
            "changepacks",
            "--yes",
            "--message",
            "test",
            "--update-type",
            "patch",
        ]);
        assert!(cli.command.is_none());
        assert!(cli.yes);
        assert_eq!(cli.message, Some("test".to_string()));
        assert!(matches!(cli.update_type, Some(CliUpdateType::Patch)));
    }

    #[test]
    fn test_cli_parsing_with_filter() {
        use clap::Parser;
        let cli = Cli::parse_from(["changepacks", "--filter", "package"]);
        assert!(cli.command.is_none());
        assert!(matches!(cli.filter, Some(FilterOptions::Package)));
    }

    #[test]
    fn test_cli_parsing_with_remote() {
        use clap::Parser;
        let cli = Cli::parse_from(["changepacks", "--remote"]);
        assert!(cli.remote);
    }
}
