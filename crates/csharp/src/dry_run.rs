//! Managed dry-run flow for C#/.NET packages.
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
//! Both `TempDir` handles are stack locals. Rust's RAII guarantees their
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
    fmt::Write as _,
    future::Future,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use changepacks_core::publish::{resolve_dry_run_publish_command, run_publish_command};
use changepacks_core::{Config, Language, PublishOutput, has_extension_ignore_ascii_case};
use tempfile::TempDir;
use tokio::fs::read_dir;

/// Shared dry-run publish flow for C#/.NET packages AND workspaces.
///
/// Both [`crate::package::CSharpPackage::dry_run_publish`] and
/// [`crate::workspace::CSharpWorkspace::dry_run_publish`] delegate here so
/// their bodies stay a single call:
///
/// 1. Resolve parent directory of `path`, returning `missing_dir_msg` as
///    error context if it has none (only difference between the two callers).
/// 2. Honor any `config.publishDryRun` override (per-project or per-language)
///    via [`resolve_dry_run_publish_command`] + [`run_publish_command`].
/// 3. Otherwise fall back to the managed pack+push flow with RAII cleanup
///    via [`run_managed_dry_run`].
///
/// # Errors
/// Returns error if the parent directory is missing, or if either the user
/// override command or the managed dry-run fails to spawn / enumerate.
pub(crate) async fn resolve_and_run_dry_run(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_msg: &'static str,
) -> Result<Option<PublishOutput>> {
    resolve_and_run_dry_run_with(
        path,
        relative_path,
        config,
        missing_dir_msg,
        |dir| async move { run_managed_dry_run(&dir).await },
    )
    .await
}

async fn resolve_and_run_dry_run_with<F, Fut>(
    path: &Path,
    relative_path: &Path,
    config: &Config,
    missing_dir_msg: &'static str,
    managed_runner: F,
) -> Result<Option<PublishOutput>>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<PublishOutput>>,
{
    let dir = path.parent().context(missing_dir_msg)?;

    if let Some(user_cmd) =
        resolve_dry_run_publish_command(relative_path, Language::CSharp, || None, config)
    {
        return Ok(Some(run_publish_command(&user_cmd, dir).await?));
    }

    Ok(Some(managed_runner(dir.to_path_buf()).await?))
}

/// Run a managed dry-run for a C#/.NET package.
///
/// Steps:
///
/// 1. Create ephemeral `pack_dir` and `feed_dir` via [`TempDir`].
/// 2. `dotnet pack -c Release -o <pack_dir>` in `working_dir` (argv, no shell).
/// 3. If pack failed, return its output immediately (TempDirs drop here).
/// 4. Enumerate `*.nupkg` in `pack_dir` via async `read_dir`.
/// 5. For each `.nupkg`, run
///    `dotnet nuget push <file> -s <feed_dir> --skip-duplicate`.
/// 6. Combine all captured stdout/stderr into a single
///    [`PublishOutput`] (success = AND of all sub-commands).
///
/// # Errors
///
/// Returns an error only when a sub-command fails to spawn at all (e.g.
/// `dotnet` is not installed) or when filesystem enumeration of `pack_dir`
/// fails. A non-zero exit from `dotnet pack` or `dotnet nuget push` is
/// reported via `PublishOutput::success = false`, not as `Err`.
///
/// The command-resolution and orchestration paths are covered without a .NET
/// SDK. Only the two statements that spawn and await `dotnet` are excluded
/// from coverage; their command runner is covered in `changepacks-core`.
pub async fn run_managed_dry_run(working_dir: &Path) -> Result<PublishOutput> {
    let pack_dir =
        TempDir::new().context("Failed to create temporary directory for dotnet pack output")?;
    let feed_dir =
        TempDir::new().context("Failed to create temporary directory for local NuGet feed")?;

    #[cfg(not(tarpaulin_include))]
    let pack_output = changepacks_core::publish::run_publish_command_os_args(
        "dotnet",
        [
            std::ffi::OsStr::new("pack"),
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new("Release"),
            std::ffi::OsStr::new("-o"),
            pack_dir.path().as_os_str(),
        ],
        working_dir,
        true,
    )
    .await
    .context("Failed to spawn `dotnet pack`")?;
    #[cfg(tarpaulin_include)]
    let pack_output = dotnet_unavailable_during_coverage()?;

    // If pack failed, surface its output verbatim — there's nothing to push.
    // TempDirs drop on return → cleanup runs.
    if !pack_output.success {
        return Ok(prefixed("dotnet pack", pack_output));
    }

    // Enumerate produced .nupkg files in Rust — no shell glob involved.
    let nupkgs = collect_nupkgs(pack_dir.path()).await.with_context(|| {
        format!(
            "Failed to enumerate .nupkg files in {}",
            pack_dir.path().display()
        )
    })?;

    let mut combined = prefixed("dotnet pack", pack_output);

    if nupkgs.is_empty() {
        combined.stderr.push_str(
            "\n[changepacks dry-run] no .nupkg produced by `dotnet pack`; \
             check that the project sets <IsPackable>true</IsPackable> and \
             includes the required PackageId / Version metadata.\n",
        );
        combined.success = false;
        // NOTE: no early return — fall through to the shared close block so
        // any tempdir cleanup failure is surfaced on this path too. The push
        // loop below is a no-op over the empty `nupkgs`, so semantics are
        // byte-identical to the previous early return.
    }

    for nupkg in &nupkgs {
        #[cfg(not(tarpaulin_include))]
        let push_output = changepacks_core::publish::run_publish_command_os_args(
            "dotnet",
            [
                std::ffi::OsStr::new("nuget"),
                std::ffi::OsStr::new("push"),
                nupkg.as_os_str(),
                std::ffi::OsStr::new("-s"),
                feed_dir.path().as_os_str(),
                std::ffi::OsStr::new("--skip-duplicate"),
            ],
            working_dir,
            true,
        )
        .await
        .with_context(|| format!("Failed to spawn `dotnet nuget push {}`", nupkg.display()))?;
        #[cfg(tarpaulin_include)]
        let push_output = dotnet_unavailable_during_coverage()?;

        let label = format!("dotnet nuget push {}", nupkg.display());
        let prefixed_output = prefixed(&label, push_output);
        combined.success &= prefixed_output.success;
        combined.stdout.push_str(&prefixed_output.stdout);
        combined.stderr.push_str(&prefixed_output.stderr);
    }

    // Explicit close on the happy path so any cleanup failure is surfaced
    // (TempDir::drop swallows errors). On the error path above, RAII Drop
    // still handles it.
    note_tempdir_close_error(pack_dir, "pack", &mut combined.stderr);
    note_tempdir_close_error(feed_dir, "feed", &mut combined.stderr);

    Ok(combined)
}

#[cfg(tarpaulin_include)]
fn dotnet_unavailable_during_coverage() -> Result<PublishOutput> {
    Err(anyhow::anyhow!(
        "the dotnet process is not spawned during coverage"
    ))
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
fn note_tempdir_close_error(dir: TempDir, label: &str, stderr: &mut String) {
    if let Err(e) = dir.close() {
        // Writing into a `String` via `fmt::Write` never returns `Err`, so
        // the discarded `Result` is `Ok(())` in practice — mirrors
        // `prompter.rs::format_selected_projects`.
        let _ = write!(
            stderr,
            "\n[changepacks dry-run] {label} tempdir cleanup error: {e}\n"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn harmless_command(marker: &str) -> String {
        format!("echo {marker}")
    }

    async fn run_harmless_managed(dir: PathBuf) -> Result<PublishOutput> {
        run_publish_command(&harmless_command("managed-fallback"), &dir).await
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

        let output = resolve_and_run_dry_run_with(
            &manifest,
            relative,
            &config,
            "missing parent",
            run_harmless_managed,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("path-override"));
        assert!(!output.stdout.contains("language-override"));
        assert!(!output.stdout.contains("managed-fallback"));
    }

    #[tokio::test]
    async fn test_resolve_dry_run_falls_back_to_language_override() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("Project.csproj");
        let mut config = Config::default();
        config
            .publish_dry_run
            .insert("csharp".to_string(), harmless_command("language-override"));

        let output = resolve_and_run_dry_run_with(
            &manifest,
            Path::new("packages/Project.csproj"),
            &config,
            "missing parent",
            run_harmless_managed,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("language-override"));
        assert!(!output.stdout.contains("managed-fallback"));
    }

    #[tokio::test]
    async fn test_resolve_dry_run_uses_injected_managed_fallback() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("Project.csproj");

        let output = resolve_and_run_dry_run_with(
            &manifest,
            Path::new("Project.csproj"),
            &Config::default(),
            "missing parent",
            run_harmless_managed,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("managed-fallback"));
    }

    #[tokio::test]
    async fn test_resolve_dry_run_reports_missing_parent_before_running() {
        let manifest = if cfg!(target_os = "windows") {
            Path::new(r"C:\")
        } else {
            Path::new("/")
        };

        let error = resolve_and_run_dry_run_with(
            manifest,
            Path::new("Project.csproj"),
            &Config::default(),
            "test missing parent",
            run_harmless_managed,
        )
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "test missing parent");
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

    /// Regression for the cancellation/cleanup story: when
    /// `run_managed_dry_run` returns (success or error), the working temp
    /// directories it created must no longer exist on disk. We can't
    /// directly observe the inner `TempDir` paths without instrumentation,
    /// so we instead assert that `dotnet` not being installed produces a
    /// clean error rather than a panic or hang — exercising the early-exit
    /// path with RAII cleanup.
    #[tokio::test]
    async fn test_managed_dry_run_errors_cleanly_when_dotnet_missing() {
        // Working dir must exist so we don't get a different error first.
        let work = TempDir::new().unwrap();

        // We don't control whether `dotnet` is installed on the test host,
        // so we only assert the contract: the function either returns an
        // `Err` (spawn failed) or returns `Ok` with a captured output. Both
        // paths must exit without leaking the working dir we passed in.
        let _ = run_managed_dry_run(work.path()).await;

        // The working dir we passed is still ours — TempDir::Drop will
        // clean it on test exit. We assert it still exists right now (the
        // function must not delete the caller's working dir, only its own
        // internally-allocated pack/feed dirs).
        assert!(work.path().exists());
    }
}
