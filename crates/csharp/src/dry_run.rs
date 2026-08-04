//! Managed real and dry-run publish flows for C#/.NET packages.
//!
//! `dotnet nuget push` has no built-in `--dry-run`, so we follow the spirit of
//! Java's `publishToMavenLocal` precedent but go one step further by running
//! the entire `pack` + `push` flow against ephemeral local directories that
//! are RAII-cleaned via [`tempfile::TempDir`].
//!
//! ## Why this avoids the shell-quoting / glob pitfalls
//!
//! Both `dotnet pack` and `dotnet nuget push` are spawned via
//! [`run_publish_command_os_args`], which uses `tokio::process::Command::args`
//! directly — no shell, no quoting bugs, no platform-specific globbing. The
//! `.nupkg` enumeration between the two steps is done in Rust via
//! [`tokio::fs::read_dir`].
//!
//! ## Why cleanup survives every failure mode
//!
//! The managed flow's `TempDir` handles are stack locals. Rust's RAII guarantees their
//! `Drop` runs on:
//!
//! - normal return,
//! - `?` error propagation,
//! - `panic!` unwind (the workspace builds with `panic = "unwind"`),
//! - future cancellation (caller drops the future mid-`.await`).
//!
//! `run_publish_command_os_args` is called with `kill_on_drop = true`, so a
//! cancelled future also terminates the child `dotnet` process before its
//! `Child` handle is dropped — preventing the Windows case where a running
//! child holds a directory open and silently defeats `remove_dir_all`.

use std::{
    ffi::OsString,
    fmt::Write as _,
    future::Future,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use changepacks_core::publish::{
    lookup_by_path_or_language, resolve_dry_run_publish_command, run_publish_command,
};
use changepacks_core::{Config, Language, PublishOutput, has_extension_ignore_ascii_case};
use tempfile::TempDir;
use tokio::fs::read_dir;

#[derive(Clone, Copy)]
enum ManagedPublishTarget {
    /// Let `dotnet nuget push` resolve its source and credentials from the
    /// user's normal `NuGet` configuration.
    UserConfig,
    /// Redirect pushes to an ephemeral filesystem feed for dry-run safety.
    TemporaryFeed,
}

impl ManagedPublishTarget {
    const fn operation(self) -> &'static str {
        match self {
            Self::UserConfig => "publish",
            Self::TemporaryFeed => "dry-run",
        }
    }
}

async fn run_managed_publish_with<F, Fut>(
    working_dir: &Path,
    manifest: &Path,
    target: ManagedPublishTarget,
    mut runner: F,
) -> Result<PublishOutput>
where
    F: FnMut(&'static str, Vec<OsString>, PathBuf) -> Fut,
    Fut: Future<Output = Result<PublishOutput>>,
{
    let manifest_arg = manifest
        .file_name()
        .context("C# project manifest path has no file name")?
        .to_owned();
    let pack_dir =
        TempDir::new().context("Failed to create temporary directory for dotnet pack output")?;
    let feed_dir = match target {
        ManagedPublishTarget::UserConfig => None,
        ManagedPublishTarget::TemporaryFeed => Some(
            TempDir::new().context("Failed to create temporary directory for local NuGet feed")?,
        ),
    };
    let pack_output = runner(
        "dotnet",
        vec![
            OsString::from("pack"),
            manifest_arg,
            OsString::from("-c"),
            OsString::from("Release"),
            OsString::from("-o"),
            pack_dir.path().as_os_str().to_owned(),
        ],
        working_dir.to_path_buf(),
    )
    .await
    .context("Failed to spawn `dotnet pack`")?;

    let mut combined = prefixed("dotnet pack", pack_output);

    if combined.success {
        let nupkgs = collect_nupkgs(pack_dir.path()).await.with_context(|| {
            format!(
                "Failed to enumerate .nupkg files in {}",
                pack_dir.path().display()
            )
        })?;

        if nupkgs.is_empty() {
            write!(
                combined.stderr,
                "\n[changepacks {}] no .nupkg produced by `dotnet pack`; \
                 check that the project sets <IsPackable>true</IsPackable> and \
                 includes the required PackageId / Version metadata.\n",
                target.operation(),
            )
            .expect("writing into a String via fmt::Write is infallible");
            combined.success = false;
        }

        for nupkg in nupkgs {
            let mut push_args = vec![
                OsString::from("nuget"),
                OsString::from("push"),
                nupkg.as_os_str().to_owned(),
            ];
            if let Some(feed_dir) = &feed_dir {
                push_args.push(OsString::from("-s"));
                push_args.push(feed_dir.path().as_os_str().to_owned());
            }
            push_args.push(OsString::from("--skip-duplicate"));
            let push_output = runner("dotnet", push_args, working_dir.to_path_buf())
                .await
                .with_context(|| {
                    format!("Failed to spawn `dotnet nuget push {}`", nupkg.display())
                })?;
            let label = format!("dotnet nuget push {}", nupkg.display());
            let prefixed_output = prefixed(&label, push_output);
            combined.success &= prefixed_output.success;
            combined.stdout.push_str(&prefixed_output.stdout);
            combined.stderr.push_str(&prefixed_output.stderr);
        }
    }

    note_tempdir_close_error(pack_dir, target.operation(), "pack", &mut combined.stderr);
    if let Some(feed_dir) = feed_dir {
        note_tempdir_close_error(feed_dir, target.operation(), "feed", &mut combined.stderr);
    }

    Ok(combined)
}

/// Shared body of the real-publish and dry-run resolvers: locate the project
/// directory, honour an already-resolved config override, and otherwise hand
/// the directory to the managed `NuGet` flow.
///
/// The two callers — [`resolve_and_run_publish_with_command_runner`] and
/// [`resolve_and_run_dry_run_with_command_runner`] — differ only in which
/// override map they consult and in the `Option` wrapper on the result, so the
/// override lookup is resolved by the caller and passed in as
/// `override_command`.
async fn resolve_and_run_with<F, Fut>(
    path: &Path,
    missing_dir_msg: &'static str,
    override_command: Option<String>,
    managed_runner: F,
) -> Result<PublishOutput>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<PublishOutput>>,
{
    let dir = path.parent().context(missing_dir_msg)?;

    if let Some(user_cmd) = override_command {
        return run_publish_command(&user_cmd, dir).await;
    }

    managed_runner(dir.to_path_buf()).await
}

/// Resolve a configured real-publish override or run the managed `NuGet` flow
/// with the supplied command boundary.
pub(crate) async fn resolve_and_run_publish_with_command_runner<F, Fut>(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_msg: &'static str,
    runner: F,
) -> Result<PublishOutput>
where
    F: FnMut(&'static str, Vec<OsString>, PathBuf) -> Fut,
    Fut: Future<Output = Result<PublishOutput>>,
{
    let manifest = path.to_path_buf();
    resolve_and_run_with(
        path,
        missing_dir_msg,
        lookup_by_path_or_language(&config.publish, relative_path, Language::CSharp).cloned(),
        move |dir| async move {
            run_managed_publish_with(&dir, &manifest, ManagedPublishTarget::UserConfig, runner)
                .await
        },
    )
    .await
}

/// Resolve a configured dry-run override or run the managed temporary-feed
/// flow with the supplied command boundary.
pub(crate) async fn resolve_and_run_dry_run_with_command_runner<F, Fut>(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_msg: &'static str,
    runner: F,
) -> Result<Option<PublishOutput>>
where
    F: FnMut(&'static str, Vec<OsString>, PathBuf) -> Fut,
    Fut: Future<Output = Result<PublishOutput>>,
{
    let manifest = path.to_path_buf();
    Ok(Some(
        resolve_and_run_with(
            path,
            missing_dir_msg,
            resolve_dry_run_publish_command(relative_path, Language::CSharp, || None, config),
            move |dir| async move {
                run_managed_publish_with(
                    &dir,
                    &manifest,
                    ManagedPublishTarget::TemporaryFeed,
                    runner,
                )
                .await
            },
        )
        .await?,
    ))
}

/// External process boundary for the otherwise deterministic managed flow.
///
/// Spawning is driven directly rather than hidden behind a coverage
/// substitution: `test_run_dotnet_command_surfaces_spawn_failure` reaches this
/// boundary with a program name no machine can resolve, so the failure path is
/// exercised for real and needs no .NET SDK to be installed.
pub(crate) async fn run_dotnet_command(
    program: &'static str,
    args: Vec<OsString>,
    working_dir: PathBuf,
) -> Result<PublishOutput> {
    changepacks_core::publish::run_publish_command_os_args(program, args, &working_dir, true).await
}

/// Asynchronously enumerate `*.nupkg` files in `dir` (non-recursive).
async fn collect_nupkgs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = read_dir(dir).await?;
    let mut out = Vec::with_capacity(4);
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();
        let is_nupkg = has_extension_ignore_ascii_case(Path::new(&file_name), "nupkg");
        // Symbol packages (`.snupkg`) never satisfy the `is_nupkg` extension
        // check above — the extension is literally `"snupkg"`, which is never
        // equal-ignore-case to `"nupkg"` (different lengths). So the extension
        // filter alone is sufficient; no explicit `is_snupkg` guard needed.
        // `dotnet nuget push` would otherwise reject them as primary packages.
        if is_nupkg {
            out.push(entry.path());
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Prefix every captured chunk with a section header so a combined
/// `PublishOutput` remains diagnosable.
fn prefixed(label: &str, mut output: PublishOutput) -> PublishOutput {
    prefix_stream(&mut output.stdout, label, "stdout");
    prefix_stream(&mut output.stderr, label, "stderr");
    output
}

fn prefix_stream(stream: &mut String, label: &str, kind: &str) {
    if !stream.is_empty() {
        let body = std::mem::take(stream);
        *stream = format!("===== {label} ({kind}) =====\n{body}");
        if !stream.ends_with('\n') {
            stream.push('\n');
        }
    }
}

/// Close a [`TempDir`] on the happy path and, if cleanup fails, append a
/// labeled note to the combined `stderr` capture. Both `pack_dir` and
/// `feed_dir` used byte-identical inline blocks before this helper existed
/// — extracting them keeps the note format in a single place so future
/// edits (label prefix, newline handling, error shape) land once instead
/// of drifting between two call sites.
fn note_tempdir_close_error(dir: TempDir, operation: &str, label: &str, stderr: &mut String) {
    note_cleanup_result(dir.close(), operation, label, stderr);
}

fn note_cleanup_result(
    result: std::io::Result<()>,
    operation: &str,
    label: &str,
    stderr: &mut String,
) {
    if let Err(e) = result {
        // `fmt::Write for String` is infallible: its `write_str` only calls
        // `String::push_str` and always returns `Ok(())`. The `expect`
        // documents that invariant instead of silently discarding the
        // `Result` — mirrors `prompter.rs::format_selected_projects`.
        write!(
            stderr,
            "\n[changepacks {operation}] {label} tempdir cleanup error: {e}\n"
        )
        .expect("writing into a String via fmt::Write is infallible");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        fs,
        sync::{Arc, Mutex},
    };
    use tempfile::TempDir;

    fn harmless_command(marker: &str) -> String {
        format!("echo {marker}")
    }

    /// Reads the `-o <dir>` output directory out of a recorded `dotnet pack` argument list.
    fn pack_output_dir(args: &[OsString]) -> PathBuf {
        let output_index = args
            .iter()
            .position(|arg| arg == "-o")
            .expect("dotnet pack args must carry an -o output directory");
        let output_dir = args
            .get(output_index + 1)
            .expect("dotnet pack -o must be followed by a directory");
        PathBuf::from(output_dir)
    }

    #[tokio::test]
    async fn test_resolve_dry_run_prefers_path_override() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("Project.csproj");
        let relative = Path::new("packages/Project.csproj");
        let mut config = Config::default();
        config
            .publish_dry_run
            .insert("csharp".to_string(), harmless_command("language-override"));
        config.publish_dry_run.insert(
            relative.to_string_lossy().into_owned(),
            harmless_command("path-override"),
        );
        let invoked = Arc::new(Mutex::new(false));
        let runner_invoked = Arc::clone(&invoked);

        let output = resolve_and_run_dry_run_with_command_runner(
            &manifest,
            relative,
            &config,
            "missing parent",
            move |_program, _args, _working_dir| {
                let runner_invoked = Arc::clone(&runner_invoked);
                async move {
                    *runner_invoked.lock().unwrap() = true;
                    Ok(PublishOutput {
                        success: true,
                        stdout: "managed-fallback".to_string(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("path-override"));
        assert!(!output.stdout.contains("language-override"));
        assert!(
            !*invoked.lock().unwrap(),
            "a matching dry-run override must bypass the managed dotnet flow"
        );
    }

    #[tokio::test]
    async fn test_resolve_publish_prefers_path_override_and_bypasses_managed_default() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("Project.csproj");
        let relative = Path::new("packages/Project.csproj");
        let mut config = Config::default();
        config
            .publish
            .insert("csharp".to_string(), harmless_command("language-override"));
        config.publish.insert(
            relative.to_string_lossy().into_owned(),
            harmless_command("path-override"),
        );
        let invoked = Arc::new(Mutex::new(false));
        let runner_invoked = Arc::clone(&invoked);

        let output = resolve_and_run_publish_with_command_runner(
            &manifest,
            relative,
            &config,
            "missing parent",
            move |_program, _args, _working_dir| {
                let runner_invoked = Arc::clone(&runner_invoked);
                async move {
                    *runner_invoked.lock().unwrap() = true;
                    Ok(PublishOutput {
                        success: true,
                        stdout: "managed-fallback".to_string(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("path-override"));
        assert!(!output.stdout.contains("language-override"));
        assert!(
            !*invoked.lock().unwrap(),
            "a matching publish override must bypass the managed dotnet flow"
        );
    }

    #[tokio::test]
    async fn test_resolve_dry_run_falls_back_to_language_override() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("Project.csproj");
        let mut config = Config::default();
        config
            .publish_dry_run
            .insert("csharp".to_string(), harmless_command("language-override"));
        let invoked = Arc::new(Mutex::new(false));
        let runner_invoked = Arc::clone(&invoked);

        let output = resolve_and_run_dry_run_with_command_runner(
            &manifest,
            Path::new("packages/Project.csproj"),
            &config,
            "missing parent",
            move |_program, _args, _working_dir| {
                let runner_invoked = Arc::clone(&runner_invoked);
                async move {
                    *runner_invoked.lock().unwrap() = true;
                    Ok(PublishOutput {
                        success: true,
                        stdout: "managed-fallback".to_string(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("language-override"));
        assert!(
            !*invoked.lock().unwrap(),
            "a language dry-run override must bypass the managed dotnet flow"
        );
    }

    #[tokio::test]
    async fn test_resolve_dry_run_uses_managed_temporary_feed_flow_without_override() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("Project.csproj");
        let calls = Arc::new(Mutex::new(Vec::<Vec<OsString>>::new()));
        let recorded_calls = Arc::clone(&calls);

        let output = resolve_and_run_dry_run_with_command_runner(
            &manifest,
            Path::new("Project.csproj"),
            &Config::default(),
            "missing parent",
            move |_program, args, _working_dir| {
                let recorded_calls = Arc::clone(&recorded_calls);
                async move {
                    if args.first().and_then(|arg| arg.to_str()) == Some("pack") {
                        let pack_dir = pack_output_dir(&args);
                        fs::write(pack_dir.join("only.nupkg"), b"").unwrap();
                    }
                    recorded_calls.lock().unwrap().push(args);
                    Ok(PublishOutput {
                        success: true,
                        stdout: "managed-fallback".to_string(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("managed-fallback"));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "calls: {calls:?}");
        assert_eq!(calls[0][0], OsString::from("pack"));
        assert!(
            calls[1].contains(&OsString::from("-s")),
            "the dry-run flow must push into a temporary local feed: {:?}",
            calls[1]
        );
    }

    #[tokio::test]
    async fn test_resolve_dry_run_reports_missing_parent_before_running() {
        let manifest = if cfg!(target_os = "windows") {
            Path::new(r"C:\")
        } else {
            Path::new("/")
        };
        let invoked = Arc::new(Mutex::new(false));
        let runner_invoked = Arc::clone(&invoked);

        let error = resolve_and_run_dry_run_with_command_runner(
            manifest,
            Path::new("Project.csproj"),
            &Config::default(),
            "test missing parent",
            move |_program, _args, _working_dir| {
                let runner_invoked = Arc::clone(&runner_invoked);
                async move {
                    *runner_invoked.lock().unwrap() = true;
                    Ok(PublishOutput {
                        success: true,
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "test missing parent");
        assert!(
            !*invoked.lock().unwrap(),
            "the missing-parent guard must run before any dotnet command"
        );
    }

    #[test]
    fn test_prefixed_adds_header_to_stdout_and_stderr() {
        let raw = PublishOutput {
            success: true,
            stdout: "hello".to_string(),
            stderr: "warn".to_string(),
        };
        let out = prefixed("dotnet pack", raw);
        assert!(out.stdout.starts_with("===== dotnet pack (stdout) ====="));
        assert!(out.stdout.contains("hello"));
        assert!(out.stdout.ends_with('\n'));
        assert!(out.stderr.starts_with("===== dotnet pack (stderr) ====="));
        assert!(out.stderr.contains("warn"));
        assert!(out.stderr.ends_with('\n'));
        assert!(out.success);
    }

    #[test]
    fn test_prefixed_leaves_empty_streams_alone() {
        let raw = PublishOutput {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
        };
        let out = prefixed("dotnet nuget push foo.nupkg", raw);
        assert!(out.stdout.is_empty());
        assert!(out.stderr.is_empty());
        assert!(!out.success);
    }

    #[test]
    fn test_cleanup_error_messages_cover_publish_and_dry_run_paths() {
        let mut stderr = String::new();
        note_cleanup_result(
            Err(std::io::Error::other("pack locked")),
            "publish",
            "pack",
            &mut stderr,
        );
        note_cleanup_result(
            Err(std::io::Error::other("feed locked")),
            "dry-run",
            "feed",
            &mut stderr,
        );

        assert!(stderr.contains("[changepacks publish] pack tempdir cleanup error: pack locked"));
        assert!(stderr.contains("[changepacks dry-run] feed tempdir cleanup error: feed locked"));
    }

    #[tokio::test]
    async fn test_collect_nupkgs_filters_and_sorts() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("b.nupkg"), b"").unwrap();
        fs::write(dir.path().join("a.nupkg"), b"").unwrap();
        fs::write(dir.path().join("ignore.txt"), b"").unwrap();
        // Symbol package — must be filtered out so we never push it.
        fs::write(dir.path().join("Foo.1.0.0.snupkg"), b"").unwrap();

        let found = collect_nupkgs(dir.path()).await.unwrap();
        assert_eq!(found.len(), 2, "found = {found:?}");
        assert!(found[0].ends_with("a.nupkg"));
        assert!(found[1].ends_with("b.nupkg"));
        for p in &found {
            assert_ne!(p.extension().and_then(|e| e.to_str()), Some("snupkg"));
        }
    }

    #[tokio::test]
    async fn test_collect_nupkgs_empty_dir() {
        let dir = TempDir::new().unwrap();
        let found = collect_nupkgs(dir.path()).await.unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn test_collect_nupkgs_missing_dir() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        let result = collect_nupkgs(&missing).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_managed_real_publish_packs_then_pushes_sorted_nupkgs_with_user_config() {
        let work = TempDir::new().unwrap();
        let calls = Arc::new(Mutex::new(Vec::<(String, Vec<OsString>, PathBuf)>::new()));
        let recorded_calls = Arc::clone(&calls);

        let output = run_managed_publish_with(
            work.path(),
            Path::new("Project.csproj"),
            ManagedPublishTarget::UserConfig,
            move |program, args, working_dir| {
                let recorded_calls = Arc::clone(&recorded_calls);
                async move {
                    if args.first().and_then(|arg| arg.to_str()) == Some("pack") {
                        let pack_dir = pack_output_dir(&args);
                        fs::write(pack_dir.join("b.nupkg"), b"").unwrap();
                        fs::write(pack_dir.join("a.nupkg"), b"").unwrap();
                        fs::write(pack_dir.join("symbols.snupkg"), b"").unwrap();
                    }
                    recorded_calls
                        .lock()
                        .unwrap()
                        .push((program.to_string(), args, working_dir));
                    Ok(PublishOutput {
                        success: true,
                        stdout: "ok".to_string(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3, "calls: {calls:?}");
        let pack_dir = PathBuf::from(&calls[0].1[5]);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pack"),
                OsString::from("Project.csproj"),
                OsString::from("-c"),
                OsString::from("Release"),
                OsString::from("-o"),
                pack_dir.clone().into_os_string(),
            ]
        );
        assert_eq!(
            calls[1].1,
            vec![
                OsString::from("nuget"),
                OsString::from("push"),
                pack_dir.join("a.nupkg").into_os_string(),
                OsString::from("--skip-duplicate"),
            ]
        );
        assert_eq!(
            calls[2].1,
            vec![
                OsString::from("nuget"),
                OsString::from("push"),
                pack_dir.join("b.nupkg").into_os_string(),
                OsString::from("--skip-duplicate"),
            ]
        );
        assert!(
            calls
                .iter()
                .all(|(program, _, cwd)| { program == "dotnet" && cwd == work.path() })
        );
        assert!(output.stdout.find("a.nupkg").unwrap() < output.stdout.find("b.nupkg").unwrap());
        assert!(!pack_dir.exists(), "temporary pack directory leaked");
    }

    #[tokio::test]
    async fn test_managed_publish_pack_targets_selected_manifest_among_siblings() {
        let root = TempDir::new().unwrap();
        let work = root.path().join("projects with spaces");
        fs::create_dir(&work).unwrap();
        let manifest = work.join("Selected Project.csproj");
        let sibling = work.join("Sibling Project.csproj");
        fs::write(&manifest, b"").unwrap();
        fs::write(&sibling, b"").unwrap();
        let calls = Arc::new(Mutex::new(Vec::<Vec<OsString>>::new()));
        let recorded_calls = Arc::clone(&calls);

        let output = resolve_and_run_publish_with_command_runner(
            &manifest,
            Path::new("projects with spaces/Selected Project.csproj"),
            &Config::default(),
            "missing parent",
            move |_program, args, _working_dir| {
                let recorded_calls = Arc::clone(&recorded_calls);
                async move {
                    if args.first().and_then(|arg| arg.to_str()) == Some("pack") {
                        let pack_dir = pack_output_dir(&args);
                        fs::write(pack_dir.join("only.nupkg"), b"").unwrap();
                    }
                    recorded_calls.lock().unwrap().push(args);
                    Ok(PublishOutput {
                        success: true,
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        let calls = calls.lock().unwrap();
        let pack_args = &calls[0];
        let pack_dir = pack_output_dir(pack_args);
        assert_eq!(
            pack_args,
            &vec![
                OsString::from("pack"),
                manifest.file_name().unwrap().to_owned(),
                OsString::from("-c"),
                OsString::from("Release"),
                OsString::from("-o"),
                pack_dir.into_os_string(),
            ]
        );
        assert!(!pack_args.contains(&sibling.file_name().unwrap().to_owned()));
    }

    #[tokio::test]
    async fn test_managed_publish_stops_after_pack_failure() {
        let work = TempDir::new().unwrap();
        let calls = Arc::new(Mutex::new(Vec::<Vec<OsString>>::new()));
        let recorded_calls = Arc::clone(&calls);

        let output = run_managed_publish_with(
            work.path(),
            Path::new("Project.csproj"),
            ManagedPublishTarget::UserConfig,
            move |_program, args, _working_dir| {
                let recorded_calls = Arc::clone(&recorded_calls);
                async move {
                    let is_pack = args.first().and_then(|arg| arg.to_str()) == Some("pack");
                    if is_pack {
                        let pack_dir = pack_output_dir(&args);
                        fs::write(pack_dir.join("partial.nupkg"), b"").unwrap();
                    }
                    recorded_calls.lock().unwrap().push(args);
                    Ok(PublishOutput {
                        success: !is_pack,
                        stdout: String::new(),
                        stderr: if is_pack {
                            "pack failed".to_string()
                        } else {
                            String::new()
                        },
                    })
                }
            },
        )
        .await
        .unwrap();

        assert!(!output.success);
        assert!(output.stderr.contains("pack failed"));
        assert_eq!(calls.lock().unwrap().len(), 1, "push ran after pack failed");
    }

    #[tokio::test]
    async fn test_managed_publish_reports_zero_artifacts() {
        let work = TempDir::new().unwrap();
        let calls = Arc::new(Mutex::new(0_usize));
        let recorded_calls = Arc::clone(&calls);

        let output = run_managed_publish_with(
            work.path(),
            Path::new("Project.csproj"),
            ManagedPublishTarget::UserConfig,
            move |_program, _args, _working_dir| {
                let recorded_calls = Arc::clone(&recorded_calls);
                async move {
                    *recorded_calls.lock().unwrap() += 1;
                    Ok(PublishOutput {
                        success: true,
                        stdout: "packed".to_string(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap();

        assert!(!output.success, "zero artifacts must not report success");
        assert!(
            output
                .stderr
                .contains("no .nupkg produced by `dotnet pack`")
        );
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn test_managed_publish_aggregates_partial_push_failure_and_continues() {
        let work = TempDir::new().unwrap();
        let calls = Arc::new(Mutex::new(Vec::<Vec<OsString>>::new()));
        let recorded_calls = Arc::clone(&calls);

        let output = run_managed_publish_with(
            work.path(),
            Path::new("Project.csproj"),
            ManagedPublishTarget::UserConfig,
            move |_program, args, _working_dir| {
                let recorded_calls = Arc::clone(&recorded_calls);
                async move {
                    let is_pack = args.first().and_then(|arg| arg.to_str()) == Some("pack");
                    if is_pack {
                        let pack_dir = pack_output_dir(&args);
                        fs::write(pack_dir.join("a.nupkg"), b"").unwrap();
                        fs::write(pack_dir.join("b.nupkg"), b"").unwrap();
                    }
                    let fails = args.iter().any(|arg| Path::new(arg).ends_with("a.nupkg"));
                    recorded_calls.lock().unwrap().push(args);
                    Ok(PublishOutput {
                        success: !fails,
                        stdout: if fails {
                            String::new()
                        } else {
                            "succeeded".to_string()
                        },
                        stderr: if fails {
                            "rejected".to_string()
                        } else {
                            String::new()
                        },
                    })
                }
            },
        )
        .await
        .unwrap();

        assert!(
            !output.success,
            "one failed push must fail the combined output"
        );
        assert!(output.stderr.contains("rejected"));
        assert!(output.stdout.contains("b.nupkg"));
        assert_eq!(
            calls.lock().unwrap().len(),
            3,
            "all artifacts must be pushed"
        );
    }

    #[tokio::test]
    async fn test_managed_dry_run_uses_local_feed_and_cleans_up_after_push_spawn_failure() {
        let work = TempDir::new().unwrap();
        let calls = Arc::new(Mutex::new(Vec::<Vec<OsString>>::new()));
        let recorded_calls = Arc::clone(&calls);

        let error = run_managed_publish_with(
            work.path(),
            Path::new("Project.csproj"),
            ManagedPublishTarget::TemporaryFeed,
            move |_program, args, _working_dir| {
                let recorded_calls = Arc::clone(&recorded_calls);
                async move {
                    let is_pack = args.first().and_then(|arg| arg.to_str()) == Some("pack");
                    if is_pack {
                        let pack_dir = pack_output_dir(&args);
                        fs::write(pack_dir.join("only.nupkg"), b"").unwrap();
                    }
                    recorded_calls.lock().unwrap().push(args);
                    if is_pack {
                        Ok(PublishOutput {
                            success: true,
                            stdout: "packed".to_string(),
                            stderr: String::new(),
                        })
                    } else {
                        Err(anyhow::anyhow!("runner spawn boom"))
                    }
                }
            },
        )
        .await
        .unwrap_err();

        let chain = format!("{error:#}");
        assert!(chain.contains("Failed to spawn `dotnet nuget push"));
        assert!(chain.contains("only.nupkg"));
        assert!(chain.contains("runner spawn boom"));

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "calls: {calls:?}");
        let pack_dir = PathBuf::from(&calls[0][5]);
        let feed_dir = PathBuf::from(&calls[1][4]);
        assert_eq!(
            calls[1],
            vec![
                OsString::from("nuget"),
                OsString::from("push"),
                pack_dir.join("only.nupkg").into_os_string(),
                OsString::from("-s"),
                feed_dir.clone().into_os_string(),
                OsString::from("--skip-duplicate"),
            ]
        );
        assert!(!pack_dir.exists(), "temporary pack directory leaked");
        assert!(!feed_dir.exists(), "temporary feed directory leaked");
    }

    // `run_dotnet_command` is the crate's only real process boundary, and its
    // contract is that a command which cannot be spawned surfaces as `Err`
    // rather than as a `PublishOutput` the managed flow would then fold into
    // its combined result. The program name cannot exist on any machine, so
    // the spawn fails immediately whether or not the .NET SDK is installed.
    #[tokio::test]
    async fn test_run_dotnet_command_surfaces_spawn_failure() {
        let work = TempDir::new().unwrap();

        let error = run_dotnet_command(
            "changepacks-csharp-test-program-that-cannot-exist",
            vec![OsString::from("--version")],
            work.path().to_path_buf(),
        )
        .await
        .expect_err("a program that cannot exist must not yield a PublishOutput");

        assert!(
            !format!("{error:#}").trim().is_empty(),
            "the process boundary must surface a diagnosable error"
        );
        assert_eq!(
            fs::read_dir(work.path()).unwrap().count(),
            0,
            "a failed spawn must leave the working directory untouched"
        );
    }

    #[tokio::test]
    async fn test_managed_publish_rejects_manifest_without_file_name() {
        let work = TempDir::new().unwrap();
        let invoked = Arc::new(Mutex::new(false));
        let runner_invoked = Arc::clone(&invoked);

        // `..` terminates in a parent component, so `Path::file_name` is `None`
        // and the guard must reject before any `dotnet` command is spawned.
        let error = run_managed_publish_with(
            work.path(),
            Path::new(".."),
            ManagedPublishTarget::TemporaryFeed,
            move |_program, _args, _working_dir| {
                let runner_invoked = Arc::clone(&runner_invoked);
                async move {
                    *runner_invoked.lock().unwrap() = true;
                    Ok(PublishOutput {
                        success: true,
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                }
            },
        )
        .await
        .unwrap_err();

        let chain = format!("{error:#}");
        assert!(
            chain.contains("C# project manifest path has no file name"),
            "chain: {chain}"
        );
        assert!(
            !*invoked.lock().unwrap(),
            "the command runner must not be invoked when the manifest has no file name"
        );
    }
}
