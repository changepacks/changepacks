//! # changepacks-cli
//!
//! Command-line interface for the changepacks version management tool.
//!
//! Provides clap-based argument parsing, interactive prompts via inquire, and async
//! command handlers for check, update, publish, config, and init operations. All commands
//! use the `Prompter` trait for testability and support colored terminal output.

use anyhow::Result;

use changepacks_core::UpdateType;
use clap::{Parser, Subcommand, ValueEnum};

use crate::{
    commands::{
        ChangepackArgs, CheckArgs, InitArgs, PublishArgs, UpdateArgs, handle_changepack,
        handle_check, handle_config, handle_init, handle_publish, handle_update,
    },
    options::{CliLanguage, FilterOptions},
};
pub mod commands;
mod context;
pub use context::*;
mod finders;
pub mod options;
pub mod prompter;

pub use prompter::{UserCancelled, is_user_cancelled};

/// Collect process arguments and run the CLI.
///
/// # Errors
/// Returns error if command execution fails.
pub async fn main_from_env(skip_binary: bool) -> Result<()> {
    let args: Vec<String> = if skip_binary {
        std::env::args().skip(1).collect()
    } else {
        std::env::args().collect()
    };
    main(&args).await
}

/// Exit successfully when `error` represents an intentional user cancellation.
pub fn exit_if_user_cancelled(error: &anyhow::Error) {
    if is_user_cancelled(error) {
        std::process::exit(0);
    }
}

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

    #[arg(short, long)]
    remote: bool,

    #[arg(short, long)]
    yes: bool,

    #[arg(short, long)]
    message: Option<String>,

    #[arg(short, long)]
    update_type: Option<CliUpdateType>,

    /// Filter projects by language. Can be specified multiple times to include multiple languages.
    #[arg(short, long, value_enum)]
    language: Vec<CliLanguage>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init(InitArgs),
    Check(CheckArgs),
    Update(UpdateArgs),
    #[command(about = "Change changepacks configuration")]
    Config,
    Publish(PublishArgs),
}

/// # Errors
/// Returns error if command execution fails.
pub async fn main(args: &[String]) -> Result<()> {
    let cli = Cli::parse_from(args);
    if let Some(command) = cli.command {
        match command {
            Commands::Init(args) => handle_init(&args).await?,
            Commands::Check(args) => handle_check(&args).await?,
            Commands::Update(args) => handle_update(&args).await?,
            Commands::Config => handle_config().await?,
            Commands::Publish(args) => handle_publish(&args).await?,
        }
    } else {
        handle_changepack(&ChangepackArgs {
            filter: cli.filter,
            remote: cli.remote,
            yes: cli.yes,
            message: cli.message,
            update_type: cli.update_type.map(Into::into),
            language: cli.language,
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

    // Discriminant helper for the `Commands` enum. `matches!(cli.command,
    // Some(Commands::X(_)))` cannot be case-parameterized directly because
    // the variant token is not a value, so we bounce through this small
    // Eq/Debug shadow enum instead.
    #[derive(Debug, PartialEq, Eq)]
    enum CmdKind {
        Init,
        Check,
        Update,
        Config,
        Publish,
    }

    impl From<&Commands> for CmdKind {
        fn from(c: &Commands) -> Self {
            match c {
                Commands::Init(_) => Self::Init,
                Commands::Check(_) => Self::Check,
                Commands::Update(_) => Self::Update,
                Commands::Config => Self::Config,
                Commands::Publish(_) => Self::Publish,
            }
        }
    }

    // Verify each subcommand argv routes to the matching `Commands` variant.
    #[rstest]
    #[case(&["changepacks", "init"], CmdKind::Init)]
    #[case(&["changepacks", "check"], CmdKind::Check)]
    #[case(&["changepacks", "update", "--dry-run"], CmdKind::Update)]
    #[case(&["changepacks", "config"], CmdKind::Config)]
    #[case(&["changepacks", "publish", "--dry-run"], CmdKind::Publish)]
    fn test_cli_parsing_command(#[case] args: &[&str], #[case] expected: CmdKind) {
        use clap::Parser;
        let cli = Cli::parse_from(args);
        let cmd = cli.command.expect("expected a subcommand");
        assert_eq!(CmdKind::from(&cmd), expected);
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

    // Repeated `--language` occurrences accumulate into `Vec<CliLanguage>`;
    // the parsed length must match the number of flags supplied.
    #[rstest]
    #[case(&["changepacks", "--language", "node"], 1)]
    #[case(&["changepacks", "--language", "node", "--language", "rust"], 2)]
    fn test_cli_parsing_language(#[case] args: &[&str], #[case] expected_len: usize) {
        use clap::Parser;
        let cli = Cli::parse_from(args);
        assert_eq!(cli.language.len(), expected_len);
    }
}
