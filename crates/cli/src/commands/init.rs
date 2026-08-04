use changepacks_core::Config;
use std::{
    io::{ErrorKind, Write},
    path::Path,
};
use tokio::{
    fs::{OpenOptions, create_dir_all},
    io::{AsyncWrite, AsyncWriteExt},
};

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

async fn write_claimed_config<W>(mut writer: W, config_file: &Path, contents: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let write_result = async {
        writer.write_all(contents).await?;
        writer.flush().await
    }
    .await;

    if let Err(error) = write_result {
        drop(writer);
        let write_context = format!(
            "Failed to write changepacks config {}",
            config_file.display()
        );

        return match tokio::fs::remove_file(config_file).await {
            Ok(()) => Err(error).with_context(|| write_context),
            Err(cleanup_error) => Err(error).with_context(|| {
                format!(
                    "{write_context}; additionally failed to remove incomplete config {}: {cleanup_error}",
                    config_file.display()
                )
            }),
        };
    }

    Ok(())
}

/// Refusal message emitted when `.changepacks/config.json` already exists.
///
/// Both the dry-run pre-check and the real `create_new` claim report the same
/// condition, and two tests pin the exact bytes, so the literal lives here once.
const ALREADY_INITIALIZED_ERROR: &str = "changepacks project already initialized";

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
    if args.dry_run {
        if tokio::fs::try_exists(&config_file).await.with_context(|| {
            format!(
                "Failed to check changepacks config {}",
                config_file.display()
            )
        })? {
            return Err(anyhow::anyhow!(ALREADY_INITIALIZED_ERROR));
        }

        // Dry-run skipped both the `create_dir_all` (line above) and the
        // write of `config.json` (below), so nothing has actually been
        // initialized — the message must reflect that or a user running
        // `changepacks init --dry-run` cannot distinguish the preview from a
        // real init.
        //
        // Render through a held `StdoutLock`: `println!` panics on a write
        // failure (a broken pipe from `changepacks init --dry-run | head`),
        // while `writeln!` lets the io error propagate through the existing
        // `Result<()>` signature. The lock is taken after the last `.await` so
        // it is never held across a suspension point.
        let mut out = std::io::stdout().lock();
        writeln!(
            out,
            "Would initialize changepacks project in {}",
            changepacks_dir.display()
        )?;
        return Ok(());
    }

    let contents = serde_json::to_string_pretty(&Config::default())?;
    // `create_new` is the claim: exactly one caller can create `config.json`,
    // and losing that race is the "already initialized" refusal rather than a
    // generic io failure. Every other kind keeps its path context and
    // propagates. Testing the two apart on the same `match` would need an
    // `open` failure that is neither `AlreadyExists` nor a `create_dir_all`
    // failure first, which no portable fixture can produce — so the refusal is
    // filtered ahead of one shared context site instead of living in a second
    // match arm no test can reach.
    let claimed = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_file)
        .await;
    if claimed
        .as_ref()
        .is_err_and(|error| error.kind() == ErrorKind::AlreadyExists)
    {
        return Err(anyhow::anyhow!(ALREADY_INITIALIZED_ERROR));
    }
    let file = claimed.with_context(|| {
        format!(
            "Failed to write changepacks config {}",
            config_file.display()
        )
    })?;
    write_claimed_config(file, &config_file, contents.as_bytes()).await?;
    // Same locked-stdout policy as the dry-run branch above: taken after the
    // final `.await` so no suspension point holds the lock.
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "changepacks project initialized in {}",
        changepacks_dir.display()
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support::init_git_repo;
    use clap::Parser;
    use rstest::rstest;
    use std::{
        io,
        pin::Pin,
        task::{Context as TaskContext, Poll},
    };
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

    struct FailAfterPartialWrite<W> {
        inner: W,
        wrote_partial: bool,
    }

    impl<W> FailAfterPartialWrite<W> {
        fn new(inner: W) -> Self {
            Self {
                inner,
                wrote_partial: false,
            }
        }
    }

    impl<W: AsyncWrite + Unpin> AsyncWrite for FailAfterPartialWrite<W> {
        fn poll_write(
            self: Pin<&mut Self>,
            context: &mut TaskContext<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            let this = self.get_mut();
            if this.wrote_partial {
                return Poll::Ready(Err(io::Error::other(
                    "injected failure after partial write",
                )));
            }

            let partial_len = buffer.len().min(1);
            match Pin::new(&mut this.inner).poll_write(context, &buffer[..partial_len]) {
                Poll::Ready(Ok(written)) if written > 0 => {
                    this.wrote_partial = true;
                    Poll::Ready(Ok(written))
                }
                result => result,
            }
        }

        fn poll_flush(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_flush(context)
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            context: &mut TaskContext<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
        }
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
    async fn test_failed_partial_config_write_removes_claim_for_retry() {
        let repository = temporary_repository();
        let changepacks_dir = get_changepacks_dir(repository.path())
            .expect("determine temporary changepacks directory");
        create_dir_all(&changepacks_dir)
            .await
            .expect("create changepacks directory");
        let config_file = changepacks_dir.join("config.json");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config_file)
            .await
            .expect("claim config path");
        let writer = FailAfterPartialWrite::new(file);
        let contents = serde_json::to_string_pretty(&Config::default())
            .expect("serialize default changepacks config");

        let error = write_claimed_config(writer, &config_file, contents.as_bytes())
            .await
            .expect_err("partial config write must fail");

        assert!(
            error
                .to_string()
                .contains("Failed to write changepacks config"),
            "write error retains config context: {error:#}"
        );
        assert!(
            format!("{error:#}").contains("injected failure after partial write"),
            "write error retains the original I/O failure: {error:#}"
        );
        assert!(
            !tokio::fs::try_exists(&config_file)
                .await
                .expect("check failed config claim"),
            "failed write must remove its claimed config path"
        );

        handle_init_at(&InitArgs { dry_run: false }, repository.path())
            .await
            .expect("init retry succeeds after failed claim cleanup");
    }

    // The `create_dir_all` context arm: a regular file already occupying the
    // `.changepacks` path makes directory creation fail on every platform, so
    // this pins the message a user sees when the path is unusable.
    #[tokio::test]
    async fn test_init_reports_changepacks_directory_creation_failure() {
        let repository = temporary_repository();
        let changepacks_dir = repository.path().join(".changepacks");
        std::fs::write(&changepacks_dir, "not a directory")
            .expect("occupy changepacks path with a regular file");

        let error = handle_init_at(&InitArgs { dry_run: false }, repository.path())
            .await
            .expect_err("init must fail when .changepacks is a regular file");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("Failed to create changepacks directory"),
            "directory creation failure keeps its context: {rendered}"
        );
        assert!(
            rendered.contains(&changepacks_dir.display().to_string()),
            "directory creation failure names the offending path: {rendered}"
        );
    }

    // The cleanup-failure half of `write_claimed_config`: the write fails and
    // the follow-up `remove_file` also fails (the parent directory does not
    // exist), so both failures must survive in the reported chain.
    #[tokio::test]
    async fn test_failed_config_write_reports_cleanup_failure() {
        let repository = tempdir().expect("create temporary directory");
        let config_file = repository.path().join("missing-dir").join("config.json");
        let contents = serde_json::to_string_pretty(&Config::default())
            .expect("serialize default changepacks config");

        let error = write_claimed_config(
            FailAfterPartialWrite::new(tokio::io::sink()),
            &config_file,
            contents.as_bytes(),
        )
        .await
        .expect_err("partial config write must fail");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("Failed to write changepacks config"),
            "cleanup failure keeps the write context: {rendered}"
        );
        assert!(
            rendered.contains("additionally failed to remove incomplete config"),
            "cleanup failure is reported alongside the write failure: {rendered}"
        );
        assert!(
            rendered.contains("injected failure after partial write"),
            "cleanup failure keeps the original I/O failure: {rendered}"
        );
    }

    #[tokio::test]
    async fn test_concurrent_init_creates_default_config_once() {
        let repository = temporary_repository();
        let args = InitArgs { dry_run: false };

        let (first, second) = tokio::join!(
            handle_init_at(&args, repository.path()),
            handle_init_at(&args, repository.path()),
        );
        let results = [first, second];

        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "exactly one concurrent init must succeed"
        );
        let errors = results
            .into_iter()
            .filter_map(Result::err)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].to_string(), ALREADY_INITIALIZED_ERROR);

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

        assert_eq!(error.to_string(), ALREADY_INITIALIZED_ERROR);
        assert_eq!(
            std::fs::read_to_string(config_file).expect("read preserved config"),
            existing_config
        );
    }

    // A second sequential init must take the `AlreadyExists` claim branch and
    // leave the config produced by the first init byte-for-byte intact.
    #[tokio::test]
    async fn test_init_twice_refuses_second_attempt_without_changing_config() {
        let repository = temporary_repository();
        let args = InitArgs { dry_run: false };

        handle_init_at(&args, repository.path())
            .await
            .expect("first init succeeds");
        let config_file = repository.path().join(".changepacks/config.json");
        let original_config = tokio::fs::read(&config_file)
            .await
            .expect("read config produced by first init");

        let error = handle_init_at(&args, repository.path())
            .await
            .expect_err("second init must be refused");

        assert_eq!(error.to_string(), ALREADY_INITIALIZED_ERROR);
        assert_eq!(
            tokio::fs::read(config_file)
                .await
                .expect("read config after refused second init"),
            original_config
        );
    }

    // The dry-run pre-check refuses an already-initialized repository with the
    // SAME message as the real `create_new` claim — it is a preview of that
    // refusal, so it must not print "Would initialize" instead — and, being a
    // dry run, it must still leave the existing config untouched.
    #[tokio::test]
    async fn test_init_dry_run_refuses_an_already_initialized_repository() {
        let repository = temporary_repository();

        handle_init_at(&InitArgs { dry_run: false }, repository.path())
            .await
            .expect("first init succeeds");
        let config_file = repository.path().join(".changepacks/config.json");
        let original_config = tokio::fs::read(&config_file)
            .await
            .expect("read config produced by first init");

        let error = handle_init_at(&InitArgs { dry_run: true }, repository.path())
            .await
            .expect_err("a dry-run init must be refused once the config exists");

        assert_eq!(error.to_string(), ALREADY_INITIALIZED_ERROR);
        assert_eq!(
            tokio::fs::read(config_file)
                .await
                .expect("read config after refused dry-run init"),
            original_config
        );
    }
}
