use crate::{Config, Language};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

/// Output captured from a publish command execution.
#[derive(Debug)]
pub struct PublishOutput {
    /// Whether the command exited with a zero status code
    pub success: bool,
    /// Captured stdout from the child process
    pub stdout: String,
    /// Captured stderr from the child process
    pub stderr: String,
}

/// Shared 2-step lookup: first by the project's relative path, then by the
/// language's `publish_key()`. Extracted so `resolve_publish_command` and
/// `resolve_dry_run_publish_command` share ONE resolution ladder; a future
/// change (e.g. adding an env-var override step) only needs to touch this
/// helper instead of drifting between two nearly-identical copies.
///
/// Language crates delegate their own config lookup to this helper to avoid
/// duplicating the path-first, language-fallback logic.
pub fn lookup_by_path_or_language(
    map: &HashMap<String, String>,
    relative_path: &Path,
    language: Language,
) -> Option<String> {
    if let Some(cmd) = map.get(relative_path.to_string_lossy().as_ref()) {
        return Some(cmd.clone());
    }
    map.get(language.publish_key()).cloned()
}

/// Resolve the publish command from config, language, or default.
///
/// `default_command_fn` is only invoked when neither a per-path nor
/// per-language override exists.
pub fn resolve_publish_command<F: FnOnce() -> String>(
    relative_path: &Path,
    language: Language,
    default_command_fn: F,
    config: &Config,
) -> String {
    lookup_by_path_or_language(&config.publish, relative_path, language)
        .unwrap_or_else(default_command_fn)
}

/// Resolve the dry-run publish command from config or fall back to the
/// language crate's `default_dry_run_command`.
///
/// Returns `None` when the language has no built-in dry-run command and the
/// user has not provided an override in `config.publish_dry_run`. Callers
/// should treat `None` as "dry-run not supported for this project; skip with a
/// warning" rather than as a failure.
///
/// `default_dry_run_command_fn` is a `FnOnce` closure so the language
/// crate's default is only invoked on the cache-miss path, mirroring
/// [`resolve_publish_command`].
pub fn resolve_dry_run_publish_command<F: FnOnce() -> Option<String>>(
    relative_path: &Path,
    language: Language,
    default_dry_run_command_fn: F,
    config: &Config,
) -> Option<String> {
    lookup_by_path_or_language(&config.publish_dry_run, relative_path, language)
        .or_else(default_dry_run_command_fn)
}

/// Convert a completed child-process `Output` into a `PublishOutput`.
///
/// Shared by both `run_publish_command_with_path_dirs` and
/// `run_publish_command_argv` so a future change to output handling (e.g.
/// lossy → strict UTF-8, extra logging) touches ONE place.
///
/// On the common valid-UTF-8 case we consume `output.stdout` / `output.stderr`
/// directly via `String::from_utf8`, which reuses the child's `Vec<u8>`
/// buffer as the `String` payload — zero copy of the stdout/stderr bytes.
/// On invalid UTF-8 we fall back to `String::from_utf8_lossy` for
/// byte-identical replacement-character semantics with the previous code.
fn build_publish_output(output: std::process::Output) -> PublishOutput {
    PublishOutput {
        success: output.status.success(),
        stdout: String::from_utf8(output.stdout)
            .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned()),
        stderr: String::from_utf8(output.stderr)
            .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned()),
    }
}

/// Build a platform-specific shell command.
/// Uses compile-time `#[cfg]` so only the active platform's code is compiled,
/// eliminating coverage gaps from unreachable platform branches.
#[cfg(target_os = "windows")]
fn build_shell_command(command: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("cmd");
    c.arg("/C").arg(command);
    c
}

/// Build a platform-specific shell command (Unix variant).
#[cfg(not(target_os = "windows"))]
fn build_shell_command(command: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c").arg(command);
    c
}

/// Build a `PATH` value with `extra_path_dirs` prepended to the current
/// process `PATH`, or `None` when there is nothing to prepend.
///
/// Package managers (npm, yarn, pnpm, and `bun install`) prepend each
/// `node_modules/.bin` directory to `PATH` when running package scripts.
/// `bun publish` / `bun pm pack` fail to do this (oven-sh/bun#16071, #18055,
/// #23594), so changepacks replicates it when it runs the publish command
/// itself. Kept generic here; the list of directories is language-specific
/// and supplied by the caller.
fn prepend_path_dirs(extra_path_dirs: &[PathBuf]) -> Option<std::ffi::OsString> {
    if extra_path_dirs.is_empty() {
        return None;
    }
    // Materialize the split-paths side into `Vec<PathBuf>` because
    // `std::env::split_paths` yields owned `PathBuf`s that must live for the
    // duration of the borrow below. The caller-supplied `extra_path_dirs`
    // slice already holds owned `PathBuf`s we can borrow from directly, so
    // borrowing skips the per-entry `PathBuf::clone` the previous shape paid
    // on every publish/dry-run (Node projects can carry several
    // `node_modules/.bin` ancestors from `node_modules_bin_dirs`).
    // `std::env::join_paths` accepts `IntoIterator<Item: AsRef<OsStr>>`, and
    // `&Path` satisfies `AsRef<OsStr>` directly — so the chained iterator
    // goes straight in without the previous intermediate `Vec<&Path>`
    // allocation. Joined output stays byte-identical.
    let existing_paths: Vec<PathBuf> = std::env::var_os("PATH")
        .as_ref()
        .map(|e| std::env::split_paths(e).collect())
        .unwrap_or_default();
    std::env::join_paths(
        extra_path_dirs
            .iter()
            .map(PathBuf::as_path)
            .chain(existing_paths.iter().map(PathBuf::as_path)),
    )
    .ok()
}

/// Execute a publish command in the given directory and return captured output.
///
/// # Errors
/// Returns error if the command fails to spawn (e.g., binary not found).
/// A non-zero exit code is reported via `PublishOutput::success = false`, not as an error.
pub async fn run_publish_command(command: &str, working_dir: &Path) -> Result<PublishOutput> {
    run_publish_command_with_path_dirs(command, working_dir, &[]).await
}

/// Execute a publish command like [`run_publish_command`], but prepend
/// `extra_path_dirs` to the child process `PATH`.
///
/// Language crates use this to inject their local binary directories (e.g.
/// `node_modules/.bin`) so lifecycle scripts such as a `prepare: husky` hook
/// resolve during `bun publish` / `npm publish`, working around bun not
/// adding them itself (oven-sh/bun#16071, #18055, #23594). When
/// `extra_path_dirs` is empty the inherited environment is used unchanged, so
/// this is behaviourally identical to [`run_publish_command`].
///
/// # Errors
/// Returns error if the command fails to spawn (e.g., binary not found).
/// A non-zero exit code is reported via `PublishOutput::success = false`, not as an error.
pub async fn run_publish_command_with_path_dirs(
    command: &str,
    working_dir: &Path,
    extra_path_dirs: &[PathBuf],
) -> Result<PublishOutput> {
    let mut cmd = build_shell_command(command);
    cmd.current_dir(working_dir);
    if let Some(path) = prepend_path_dirs(extra_path_dirs) {
        cmd.env("PATH", path);
    }
    let output = cmd.output().await?;
    Ok(build_publish_output(output))
}

/// Resolve the parent directory of `manifest_path` and run `command` there.
///
/// Extracted so `Package::publish` and `Workspace::publish` share a single
/// implementation. Callers pass their own `missing_dir_ctx` message
/// (`"Package directory not found"` vs `"Workspace directory not found"`) so
/// error messages match the caller's role exactly.
///
/// # Errors
/// Returns error if the command fails to spawn or `manifest_path` has no
/// parent directory.
#[cfg(not(tarpaulin_include))]
pub async fn run_publish_flow(
    command: &str,
    manifest_path: &Path,
    extra_path_dirs: &[PathBuf],
    missing_dir_ctx: &'static str,
) -> Result<PublishOutput> {
    let dir = manifest_path.parent().context(missing_dir_ctx)?;
    run_publish_command_with_path_dirs(command, dir, extra_path_dirs).await
}

/// Resolve the parent directory of `manifest_path` and run the dry-run
/// `command` there. Returns `Ok(None)` when no command is supplied, matching
/// the "dry-run not supported for this project" convention.
///
/// Extracted so `Package::dry_run_publish` and `Workspace::dry_run_publish`
/// share a single implementation.
///
/// # Errors
/// Returns error if the command fails to spawn or `manifest_path` has no
/// parent directory.
#[cfg(not(tarpaulin_include))]
pub async fn run_dry_run_publish_flow(
    command: Option<&str>,
    manifest_path: &Path,
    extra_path_dirs: &[PathBuf],
    missing_dir_ctx: &'static str,
) -> Result<Option<PublishOutput>> {
    let Some(cmd) = command else {
        return Ok(None);
    };
    let dir = manifest_path.parent().context(missing_dir_ctx)?;
    Ok(Some(
        run_publish_command_with_path_dirs(cmd, dir, extra_path_dirs).await?,
    ))
}

/// Execute a command by argv (no shell) with optional `kill_on_drop`.
///
/// Use this when callers need cross-platform argument passing without shell
/// quoting concerns (e.g., paths with spaces, wildcards that should not be
/// shell-expanded, untrusted user-supplied paths). With `kill_on_drop = true`,
/// if the returned future is cancelled the child process is terminated before
/// the `Child` handle is dropped — important when the caller relies on RAII to
/// clean up temporary directories the child has open.
///
/// # Errors
/// Returns error if the command fails to spawn. A non-zero exit code is
/// reported via `PublishOutput::success = false`, not as an error.
pub async fn run_publish_command_argv(
    program: &str,
    args: &[&str],
    working_dir: &Path,
    kill_on_drop: bool,
) -> Result<PublishOutput> {
    run_publish_command_os_args(
        program,
        args.iter().map(|arg| OsStr::new(*arg)),
        working_dir,
        kill_on_drop,
    )
    .await
}

/// Execute a command by argv with path-safe OS string arguments.
///
/// # Errors
/// Returns error if the command fails to spawn. A non-zero exit code is
/// reported via `PublishOutput::success = false`, not as an error.
pub async fn run_publish_command_os_args<I, S>(
    program: &str,
    args: I,
    working_dir: &Path,
    kill_on_drop: bool,
) -> Result<PublishOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).current_dir(working_dir);
    cmd.kill_on_drop(kill_on_drop);
    let output = cmd.output().await?;
    Ok(build_publish_output(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_resolve_publish_command_by_path() {
        let mut publish = HashMap::new();
        publish.insert(
            "packages/core/package.json".to_string(),
            "custom publish".to_string(),
        );
        let config = Config {
            publish,
            ..Default::default()
        };

        let result = resolve_publish_command(
            Path::new("packages/core/package.json"),
            Language::Node,
            || "npm publish".to_string(),
            &config,
        );
        assert_eq!(result, "custom publish");
    }

    #[test]
    fn test_resolve_publish_command_by_language() {
        let mut publish = HashMap::new();
        publish.insert(
            "node".to_string(),
            "npm publish --access public".to_string(),
        );
        let config = Config {
            publish,
            ..Default::default()
        };

        let result = resolve_publish_command(
            Path::new("package.json"),
            Language::Node,
            || "npm publish".to_string(),
            &config,
        );
        assert_eq!(result, "npm publish --access public");
    }

    #[test]
    fn test_resolve_publish_command_default_fallback() {
        let config = Config::default();

        let result = resolve_publish_command(
            Path::new("package.json"),
            Language::Node,
            || "npm publish".to_string(),
            &config,
        );
        assert_eq!(result, "npm publish");
    }

    #[test]
    fn test_resolve_dry_run_publish_command_by_path() {
        // Per-project override wins even when a default is provided.
        let mut publish_dry_run = HashMap::new();
        publish_dry_run.insert(
            "packages/core/package.json".to_string(),
            "custom dry".to_string(),
        );
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        let result = resolve_dry_run_publish_command(
            Path::new("packages/core/package.json"),
            Language::Node,
            || Some("npm publish --dry-run".to_string()),
            &config,
        );
        assert_eq!(result.as_deref(), Some("custom dry"));
    }

    #[test]
    fn test_resolve_dry_run_publish_command_by_language() {
        // Per-language override wins over the language crate's default.
        let mut publish_dry_run = HashMap::new();
        publish_dry_run.insert("node".to_string(), "npm publish --dry-run -tag".to_string());
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        let result = resolve_dry_run_publish_command(
            Path::new("package.json"),
            Language::Node,
            || Some("npm publish --dry-run".to_string()),
            &config,
        );
        assert_eq!(result.as_deref(), Some("npm publish --dry-run -tag"));
    }

    #[test]
    fn test_resolve_dry_run_publish_command_falls_back_to_language_default() {
        // No override in config: fall back to the language crate's default.
        let config = Config::default();

        let result = resolve_dry_run_publish_command(
            Path::new("package.json"),
            Language::Node,
            || Some("npm publish --dry-run".to_string()),
            &config,
        );
        assert_eq!(result.as_deref(), Some("npm publish --dry-run"));
    }

    #[test]
    fn test_resolve_dry_run_publish_command_unsupported_returns_none() {
        // When the language crate has no dry-run default (e.g. CSharp/NuGet)
        // and the user has not overridden it, the resolver returns None so
        // callers can skip with a warning.
        let config = Config::default();

        let result = resolve_dry_run_publish_command(
            Path::new("project.csproj"),
            Language::CSharp,
            || None,
            &config,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_dry_run_publish_command_unsupported_with_path_override() {
        // Per-project override still wins for unsupported languages.
        let mut publish_dry_run = HashMap::new();
        publish_dry_run.insert(
            "project.csproj".to_string(),
            "dotnet pack -c Release".to_string(),
        );
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        let result = resolve_dry_run_publish_command(
            Path::new("project.csproj"),
            Language::CSharp,
            || None,
            &config,
        );
        assert_eq!(result.as_deref(), Some("dotnet pack -c Release"));
    }

    #[test]
    fn test_resolve_dry_run_publish_command_unsupported_with_language_override() {
        // Per-language override resolves for unsupported languages too.
        let mut publish_dry_run = HashMap::new();
        publish_dry_run.insert("csharp".to_string(), "dotnet pack -c Release".to_string());
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        let result = resolve_dry_run_publish_command(
            Path::new("project.csproj"),
            Language::CSharp,
            || None,
            &config,
        );
        assert_eq!(result.as_deref(), Some("dotnet pack -c Release"));
    }

    #[tokio::test]
    async fn test_run_publish_command_success() {
        let temp_dir = std::env::temp_dir();
        let command = if cfg!(target_os = "windows") {
            "cmd /c echo publish"
        } else {
            "echo publish"
        };
        let output = run_publish_command(command, &temp_dir).await.unwrap();
        assert!(output.success);
        assert!(output.stdout.contains("publish"));
    }

    #[tokio::test]
    async fn test_run_publish_command_failure() {
        let temp_dir = std::env::temp_dir();
        let command = if cfg!(target_os = "windows") {
            "cmd /c exit 1"
        } else {
            "exit 1"
        };
        let output = run_publish_command(command, &temp_dir).await.unwrap();
        assert!(!output.success);
    }

    #[tokio::test]
    async fn test_run_publish_command_with_path_dirs_empty_is_noop() {
        // Empty extra dirs must behave exactly like `run_publish_command`.
        let temp_dir = std::env::temp_dir();
        let command = if cfg!(target_os = "windows") {
            "cmd /c echo publish"
        } else {
            "echo publish"
        };
        let output = run_publish_command_with_path_dirs(command, &temp_dir, &[])
            .await
            .unwrap();
        assert!(output.success);
        assert!(output.stdout.contains("publish"));
    }

    #[tokio::test]
    async fn test_run_publish_command_with_path_dirs_resolves_bare_binary() {
        // Reproduces the husky failure at the runner level: a bare command name
        // resolves ONLY because its directory was prepended to PATH. This is the
        // exact behaviour `bun publish` fails to provide for node_modules/.bin
        // (oven-sh/bun#16071, #18055, #23594); the injection restores it.
        let base = std::env::temp_dir().join(format!("changepacks_pathinj_{}", std::process::id()));
        let bin = base.join("bin");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&bin).unwrap();

        let hook = bin.join(if cfg!(target_os = "windows") {
            "cphook.cmd"
        } else {
            "cphook"
        });
        if cfg!(target_os = "windows") {
            std::fs::write(&hook, "@echo hook-ran\r\n").unwrap();
        } else {
            std::fs::write(&hook, "#!/bin/sh\necho hook-ran\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        // Bare command name; resolvable only via the injected PATH entry.
        let output =
            run_publish_command_with_path_dirs("cphook", &base, std::slice::from_ref(&bin))
                .await
                .unwrap();
        let _ = std::fs::remove_dir_all(&base);
        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("hook-ran"));
    }

    #[test]
    fn test_prepend_path_dirs_empty_returns_none() {
        assert!(prepend_path_dirs(&[]).is_none());
    }

    #[test]
    fn test_prepend_path_dirs_prepends_first_and_preserves_existing() {
        // The injected directory must be first; pre-existing PATH entries must
        // remain reachable afterwards (this is what lets a `prepare: husky`
        // hook resolve during `bun publish`).
        let dir = PathBuf::from(if cfg!(target_os = "windows") {
            "C:\\changepacks-path-test-bin"
        } else {
            "/changepacks-path-test-bin"
        });
        let joined = prepend_path_dirs(std::slice::from_ref(&dir)).expect("some PATH");
        let parsed: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(parsed.first(), Some(&dir));
    }

    #[tokio::test]
    async fn test_run_publish_command_argv_success() {
        let temp_dir = std::env::temp_dir();
        // `cmd.exe /C echo hi` on Windows; `/bin/echo hi` on Unix.
        let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
            ("cmd", vec!["/C", "echo", "argv-ok"])
        } else {
            ("echo", vec!["argv-ok"])
        };
        let output = run_publish_command_argv(program, &args, &temp_dir, true)
            .await
            .unwrap();
        assert!(output.success);
        assert!(output.stdout.contains("argv-ok"));
    }

    #[tokio::test]
    async fn test_run_publish_command_argv_failure() {
        let temp_dir = std::env::temp_dir();
        let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
            ("cmd", vec!["/C", "exit", "1"])
        } else {
            ("sh", vec!["-c", "exit 1"])
        };
        let output = run_publish_command_argv(program, &args, &temp_dir, true)
            .await
            .unwrap();
        assert!(!output.success);
    }

    #[tokio::test]
    async fn test_run_publish_command_argv_spawn_error() {
        let temp_dir = std::env::temp_dir();
        let result = run_publish_command_argv(
            "this-binary-does-not-exist-changepacks-test",
            &[],
            &temp_dir,
            true,
        )
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_build_shell_command() {
        let cmd = build_shell_command("echo hello");
        let program = cmd.as_std().get_program().to_string_lossy().to_string();
        #[cfg(target_os = "windows")]
        assert_eq!(program, "cmd");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(program, "sh");
    }
}
