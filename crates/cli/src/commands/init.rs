use changepacks_core::Config;
use std::path::Path;
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
pub async fn handle_init(args: &InitArgs) -> Result<()> {
    let current_dir =
        std::env::current_dir().context("Failed to determine current working directory")?;
    handle_init_at(args, &current_dir).await
}

async fn handle_init_at(args: &InitArgs, current_dir: &Path) -> Result<()> {
    // create .changepacks directory
    let changepacks_dir = get_changepacks_dir(current_dir)?;
    if !args.dry_run {
        create_dir_all(&changepacks_dir).await.with_context(|| {
            format!(
                "Failed to create changepacks directory {}",
                changepacks_dir.display()
            )
        })?;
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
                &config_file,
                serde_json::to_string_pretty(&Config::default())?,
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to write changepacks config {}",
                    config_file.display()
                )
            })?;
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
    use changepacks_utils::test_support::init_git_repo;
    use clap::Parser;
    use rstest::rstest;
    use tempfile::{TempDir, tempdir};

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

    fn temporary_repository() -> TempDir {
        let repository = tempdir().expect("create temporary repository");
        init_git_repo(repository.path());
        repository
    }

    #[tokio::test]
    async fn test_init_dry_run_does_not_modify_repository() {
        let repository = temporary_repository();
        let args = InitArgs { dry_run: true };

        handle_init_at(&args, repository.path())
            .await
            .expect("dry-run init succeeds");

        assert!(!repository.path().join(".changepacks").exists());
    }

    #[tokio::test]
    async fn test_init_creates_default_config() {
        let repository = temporary_repository();
        let args = InitArgs { dry_run: false };

        handle_init_at(&args, repository.path())
            .await
            .expect("init succeeds");

        let config = std::fs::read_to_string(repository.path().join(".changepacks/config.json"))
            .expect("read generated config");
        assert_eq!(
            config,
            concat!(
                "{\n",
                "  \"ignore\": [],\n",
                "  \"baseBranch\": \"main\",\n",
                "  \"latestPackage\": null,\n",
                "  \"publish\": {},\n",
                "  \"publishDryRun\": {},\n",
                "  \"updateOn\": {}\n",
                "}"
            )
        );
    }

    #[tokio::test]
    async fn test_init_refuses_and_preserves_existing_config() {
        let repository = temporary_repository();
        let changepacks_dir = repository.path().join(".changepacks");
        std::fs::create_dir(&changepacks_dir).expect("create changepacks directory");
        let config_file = changepacks_dir.join("config.json");
        let existing_config = "{\n  \"existing\": true\n}\n";
        std::fs::write(&config_file, existing_config).expect("write existing config");
        let args = InitArgs { dry_run: false };

        let error = handle_init_at(&args, repository.path())
            .await
            .expect_err("existing config must be refused");

        assert_eq!(error.to_string(), "changepacks project already initialized");
        assert_eq!(
            std::fs::read_to_string(config_file).expect("read preserved config"),
            existing_config
        );
    }
}
