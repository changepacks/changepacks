use changepacks_core::Config;
use tokio::fs::{create_dir_all, write};

use anyhow::{Context, Result};
use changepacks_utils::get_changepacks_dir;
use clap::Args;

#[derive(Args, Debug)]
#[command(about = "Initialize a new changepacks project")]
pub struct InitArgs {
    /// If true, do not make any filesystem changes.
    #[arg(short, long)]
    dry_run: bool,
}

/// Initialize a new changepacks project
///
/// # Errors
/// Returns error if creating the .changepacks directory or config file fails.
///
/// Excluded from coverage: filesystem I/O orchestration; the argument
/// parsing is covered separately by `test_init_args_*` tests.
#[cfg(not(tarpaulin_include))]
pub async fn handle_init(args: &InitArgs) -> Result<()> {
    // create .changepacks directory
    let current_dir =
        std::env::current_dir().context("Failed to determine current working directory")?;
    let changepacks_dir = get_changepacks_dir(&current_dir)?;
    if !args.dry_run {
        create_dir_all(&changepacks_dir).await?;
    }
    // create config.json file
    let config_file = changepacks_dir.join("config.json");
    if tokio::fs::try_exists(&config_file).await.with_context(|| {
        format!(
            "Failed to check changepacks config {}",
            config_file.display()
        )
    })? {
        Err(anyhow::anyhow!("changepacks project already initialized"))
    } else {
        if args.dry_run {
            // Dry-run skipped both the `create_dir_all` (line above) and the
            // `write` of `config.json` (line below), so nothing has actually
            // been initialized — the message must reflect that or a user
            // running `changepacks init --dry-run` cannot distinguish the
            // preview from a real init.
            println!(
                "Would initialize changepacks project in {}",
                changepacks_dir.display()
            );
        } else {
            write(
                config_file,
                serde_json::to_string_pretty(&Config::default())?,
            )
            .await?;
            println!(
                "changepacks project initialized in {}",
                changepacks_dir.display()
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use rstest::rstest;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        init: InitArgs,
    }

    #[test]
    fn test_init_args_default() {
        let cli = TestCli::parse_from(["test"]);
        assert!(!cli.init.dry_run);
    }

    // `--dry-run` (long) and `-d` (short) both flip the `dry_run` flag.
    #[rstest]
    #[case(&["test", "--dry-run"])]
    #[case(&["test", "-d"])]
    fn test_init_args_dry_run_flag(#[case] args: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert!(cli.init.dry_run);
    }
}
