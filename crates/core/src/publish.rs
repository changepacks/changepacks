use crate::{Config, Language};
use anyhow::{Context, Result};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

/// Error context when a package directory cannot be determined.
pub const PACKAGE_DIR_NOT_FOUND: &str = "Package directory not found";

/// Error context when a workspace directory cannot be determined.
pub const WORKSPACE_DIR_NOT_FOUND: &str = "Workspace directory not found";

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

/// Normalize Windows backslash path separators to forward slashes.
///
/// Config keys and `--project` values are documented and written with forward
/// slashes. This helper normalizes Windows backslashes to forward slashes so
/// that filesystem-derived paths (which carry backslashes on Windows) can be
/// compared against config keys without silent misses.
///
/// Returns [`Cow::Borrowed`] when the input already contains no backslash — the
/// case for every path on non-Windows platforms and for most already-normalized
/// paths on Windows — so the common case costs no allocation at all. Only a
/// backslash-carrying path pays for the rewritten [`Cow::Owned`] string. Call
/// `.into_owned()` at sites that genuinely need to store a `String`.
#[must_use]
pub fn normalize_path_separators(s: &str) -> Cow<'_, str> {
    if s.contains('\\') {
        Cow::Owned(s.replace('\\', "/"))
    } else {
        Cow::Borrowed(s)
    }
}

/// Normalize a [`Path`] to a forward-slash-separated string.
///
/// The `Path` counterpart of [`normalize_path_separators`], which only covers
/// the second half of the conversion. Callers that start from a `Path` had to
/// bind the [`Path::to_string_lossy`] temporary themselves (otherwise the
/// borrowed [`Cow`] handed back would point at a dropped temporary) and then
/// re-derive the same allocation policy by hand.
///
/// Allocation behaviour matches the hand-written versions this replaces:
/// matching on the lossy `Cow` moves an already-owned lossy string (a non-UTF-8
/// path) through untouched when it has no backslash, and a borrowed one is only
/// copied when it actually contains a backslash. Call `.into_owned()` at sites
/// that genuinely need to store a `String`.
#[must_use]
pub fn normalize_path_separators_of(path: &Path) -> Cow<'_, str> {
    match path.to_string_lossy() {
        // Borrowed: the path was valid UTF-8, so defer to the `&str` helper,
        // which borrows straight out of `path` unless a rewrite is needed.
        Cow::Borrowed(s) => normalize_path_separators(s),
        // Owned: `to_string_lossy` already allocated. Only pay for a second
        // allocation when there is genuinely something to rewrite.
        Cow::Owned(s) => Cow::Owned(if s.contains('\\') {
            s.replace('\\', "/")
        } else {
            s
        }),
    }
}

/// Shared 2-step lookup: first by the project's relative path, then by the
/// language's `publish_key()`. Extracted so `resolve_publish_command` and
/// `resolve_dry_run_publish_command` share ONE resolution ladder; a future
/// change (e.g. adding an env-var override step) only needs to touch this
/// helper instead of drifting between two nearly-identical copies.
///
/// Language crates delegate their own config lookup to this helper to avoid
/// duplicating the path-first, language-fallback logic.
///
/// Returns a borrow into `map` rather than an owned `String`: the hottest
/// caller (`sort_publishable_projects` in the CLI) only needs
/// `Option::is_some`, so an owning return allocated and immediately dropped a
/// command string once per candidate project. Callers that genuinely need
/// ownership opt in with an explicit `.cloned()`.
#[must_use]
pub fn lookup_by_path_or_language<'a>(
    map: &'a BTreeMap<String, String>,
    relative_path: &Path,
    language: Language,
) -> Option<&'a String> {
    // Empty publish maps can only miss; avoid path conversion and lookup/comparison work.
    if map.is_empty() {
        return None;
    }
    let lossy = relative_path.to_string_lossy();
    if let Some(cmd) = map.get(lossy.as_ref()) {
        return Some(cmd);
    }
    // Config keys are documented/written with forward slashes, but the Rust
    // finder's filesystem-derived relative paths carry backslashes on Windows.
    // Retry with forward-slash normalization if the exact lookup missed and
    // the string contains a backslash, so a forward-slash config key does not
    // silently miss. See `normalize_path_separators` for the shared normalization
    // policy used across the CLI and core. The guard already established that a
    // backslash is present, which makes that helper's own `contains` check
    // redundant here, so this replaces directly and keeps the probed key
    // byte-identical to the helper's `Cow::Owned` arm.
    if lossy.contains('\\')
        && let Some(cmd) = map.get(lossy.replace('\\', "/").as_str())
    {
        return Some(cmd);
    }
    map.get(language.publish_key())
}

/// Resolve the publish command from config, language, or default.
///
/// `default_command_fn` is only invoked when neither a per-path nor
/// per-language override exists.
#[must_use]
pub fn resolve_publish_command<F: FnOnce() -> String>(
    relative_path: &Path,
    language: Language,
    default_command_fn: F,
    config: &Config,
) -> String {
    lookup_by_path_or_language(&config.publish, relative_path, language)
        .cloned()
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
#[must_use]
pub fn resolve_dry_run_publish_command<F: FnOnce() -> Option<String>>(
    relative_path: &Path,
    language: Language,
    default_dry_run_command_fn: F,
    config: &Config,
) -> Option<String> {
    lookup_by_path_or_language(&config.publish_dry_run, relative_path, language)
        .cloned()
        .or_else(default_dry_run_command_fn)
}

/// Decode process-output bytes as UTF-8, reusing the buffer on the valid-UTF-8
/// happy path (zero-copy) and falling back to lossy replacement on invalid
/// UTF-8.
///
/// On the common valid-UTF-8 case we consume `bytes` directly via
/// `String::from_utf8`, which reuses the child's `Vec<u8>` buffer as the
/// `String` payload — zero copy of the stdout/stderr bytes. On invalid UTF-8
/// we fall back to `String::from_utf8_lossy` for byte-identical
/// replacement-character semantics with the previous code.
fn utf8_or_lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned())
}

impl From<std::process::Output> for PublishOutput {
    fn from(output: std::process::Output) -> Self {
        Self {
            success: output.status.success(),
            stdout: utf8_or_lossy(output.stdout),
            stderr: utf8_or_lossy(output.stderr),
        }
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
fn prepend_path_dirs(extra_path_dirs: &[PathBuf]) -> Result<Option<OsString>> {
    if extra_path_dirs.is_empty() {
        return Ok(None);
    }
    // Stream both sides lazily instead of materializing the process `PATH`
    // into a throwaway `Vec<PathBuf>` spine (commonly 30-60 entries) whose
    // only job was to own the `split_paths` output for the duration of the
    // borrow. Only `path_var` is bound so the `OsString` outlives the
    // `split_paths` iterator borrowing it; the entries themselves are
    // consumed one at a time. The caller-supplied `extra_path_dirs` are
    // borrowed (`Cow::Borrowed`) so no per-entry `PathBuf::clone` is paid,
    // while `split_paths` yields owned `PathBuf`s that become `Cow::Owned`
    // without a copy. `std::env::join_paths` still performs the separator
    // validation and the joined output stays byte-identical.
    let path_var = std::env::var_os("PATH");
    let existing = path_var.as_deref().map(std::env::split_paths);
    let path = std::env::join_paths(
        extra_path_dirs
            .iter()
            .map(|dir| Cow::Borrowed(dir.as_os_str()))
            .chain(
                existing
                    .into_iter()
                    .flatten()
                    .map(|dir| Cow::<OsStr>::Owned(dir.into_os_string())),
            ),
    )
    .context("failed to construct PATH from injected and existing directories")?;
    Ok(Some(path))
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
/// Returns error if the child `PATH` cannot be constructed or the command
/// fails to spawn (e.g., binary not found).
/// A non-zero exit code is reported via `PublishOutput::success = false`, not as an error.
pub async fn run_publish_command_with_path_dirs(
    command: &str,
    working_dir: &Path,
    extra_path_dirs: &[PathBuf],
) -> Result<PublishOutput> {
    let mut cmd = build_shell_command(command);
    cmd.current_dir(working_dir);
    if let Some(path) = prepend_path_dirs(extra_path_dirs)? {
        cmd.env("PATH", path);
    }
    cmd.kill_on_drop(true);
    let output = cmd.output().await?;
    Ok(output.into())
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

/// Execute a command by argv with path-safe OS string arguments.
///
/// Use this when callers need cross-platform argument passing without shell
/// quoting concerns (e.g., paths with spaces, wildcards that should not be
/// shell-expanded, untrusted user-supplied paths). With `kill_on_drop = true`,
/// if the returned future is cancelled the child process is terminated before
/// the `Child` handle is dropped.
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
    Ok(output.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    async fn wait_for_file(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if tokio::fs::metadata(path).await.is_ok() {
                return true;
            }
            tokio::task::yield_now().await;
        }
        false
    }

    #[test]
    fn test_lookup_by_path_or_language_empty_map_returns_none() {
        // `lookup_by_path_or_language` is a public cross-crate API (used
        // directly by changepacks-node and changepacks-java), so its empty-map
        // contract is pinned here rather than only through the two resolvers.
        // The control assertion proves the path/language pair really would
        // match, so the `None` below is caused by emptiness alone and the
        // `map.is_empty()` fast path stays behaviour-preserving.
        let path = Path::new("packages/core/package.json");
        let populated = BTreeMap::from([(
            "packages/core/package.json".to_string(),
            "custom publish".to_string(),
        )]);
        assert_eq!(
            lookup_by_path_or_language(&populated, path, Language::Node).map(String::as_str),
            Some("custom publish")
        );

        let empty = BTreeMap::new();
        assert!(lookup_by_path_or_language(&empty, path, Language::Node).is_none());
        // Same for a key that would resolve via the language fallback.
        assert!(
            lookup_by_path_or_language(&empty, Path::new("package.json"), Language::Node).is_none()
        );
    }

    #[test]
    fn test_lookup_by_path_or_language_path_key_wins_over_language_key() {
        // Both rungs of the ladder are present in ONE map: the exact
        // repo-relative path must win over the language `publish_key`.
        let map = BTreeMap::from([
            (
                "packages/core/package.json".to_string(),
                "path publish".to_string(),
            ),
            ("node".to_string(), "language publish".to_string()),
        ]);

        let result = lookup_by_path_or_language(
            &map,
            Path::new("packages/core/package.json"),
            Language::Node,
        );
        assert_eq!(result.map(String::as_str), Some("path publish"));
    }

    #[test]
    fn test_lookup_by_path_or_language_backslash_path_matches_forward_slash_key() {
        // Windows finders hand back backslash-separated relative paths while
        // config keys are documented with forward slashes. The normalization
        // retry must resolve it, and must do so BEFORE the language fallback —
        // the distinct `language publish` value below is what proves the retry
        // ran instead of the map.get(publish_key) rung.
        let map = BTreeMap::from([
            (
                "packages/core/package.json".to_string(),
                "path publish".to_string(),
            ),
            ("node".to_string(), "language publish".to_string()),
        ]);

        let result = lookup_by_path_or_language(
            &map,
            Path::new("packages\\core\\package.json"),
            Language::Node,
        );
        assert_eq!(result.map(String::as_str), Some("path publish"));
    }

    #[test]
    fn test_lookup_by_path_or_language_falls_back_to_language_key() {
        // No path key matches, so the language `publish_key` entry is returned.
        let map = BTreeMap::from([
            (
                "packages/other/package.json".to_string(),
                "other publish".to_string(),
            ),
            ("node".to_string(), "language publish".to_string()),
        ]);

        let result = lookup_by_path_or_language(
            &map,
            Path::new("packages/core/package.json"),
            Language::Node,
        );
        assert_eq!(result.map(String::as_str), Some("language publish"));
    }

    #[test]
    fn test_lookup_by_path_or_language_returns_none_when_neither_matches() {
        // Non-empty map, but neither the path nor the language key is present:
        // the ladder falls off its last rung and yields None. The `rust` entry
        // guards against a fallback that ignores the requested language.
        let map = BTreeMap::from([
            (
                "packages/other/package.json".to_string(),
                "other publish".to_string(),
            ),
            ("rust".to_string(), "cargo publish".to_string()),
        ]);

        let result = lookup_by_path_or_language(
            &map,
            Path::new("packages/core/package.json"),
            Language::Node,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_publish_command_by_path() {
        let mut publish = BTreeMap::new();
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
    fn test_resolve_publish_command_by_path_backslash_separators() {
        // Windows backslash → forward-slash config-key normalization.
        let mut publish = BTreeMap::new();
        publish.insert(
            "packages/core/package.json".to_string(),
            "custom publish".to_string(),
        );
        let config = Config {
            publish,
            ..Default::default()
        };

        let result = resolve_publish_command(
            Path::new("packages\\core\\package.json"),
            Language::Node,
            || "npm publish".to_string(),
            &config,
        );
        assert_eq!(result, "custom publish");
    }

    #[test]
    fn test_resolve_publish_command_by_language() {
        let mut publish = BTreeMap::new();
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
        let mut publish_dry_run = BTreeMap::new();
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
        let mut publish_dry_run = BTreeMap::new();
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
        let mut publish_dry_run = BTreeMap::new();
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
        let mut publish_dry_run = BTreeMap::new();
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
    async fn test_run_publish_command_cancellation_kills_shell_child() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "changepacks_shell_cancel_{}_{}",
            std::process::id(),
            unique
        ));
        std::fs::create_dir_all(&base).unwrap();
        let started = base.join("started");
        let completed = base.join("completed");

        #[cfg(target_os = "windows")]
        let command = "echo started>started & ping -n 3 127.0.0.1 >nul & echo completed>completed"
            .to_string();
        #[cfg(not(target_os = "windows"))]
        let command =
            "printf '%s' \"$$\" > pid; printf started > started; sleep 2; printf completed > completed"
                .to_string();

        let task_dir = base.clone();
        let mut task = tokio::spawn(async move { run_publish_command(&command, &task_dir).await });
        tokio::task::yield_now().await;
        let started_written = wait_for_file(&started, Duration::from_secs(5)).await;
        if !started_written && task.is_finished() {
            panic!(
                "shell command finished before readiness: {:?}",
                (&mut task).await
            );
        }
        assert!(started_written, "shell child never wrote its start marker");

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        #[cfg(unix)]
        {
            let pid = std::fs::read_to_string(base.join("pid")).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut exited = false;
            while Instant::now() < deadline {
                let running = tokio::process::Command::new("kill")
                    .args(["-0", pid.trim()])
                    .status()
                    .await
                    .is_ok_and(|status| status.success());
                if !running {
                    exited = true;
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(exited, "cancelled shell child {pid} did not exit");
        }

        // The command writes `completed` after two seconds (roughly two seconds
        // of ping on Windows). Polling for longer proves cancellation prevented
        // that delayed side effect without racing the start of the child.
        assert!(
            !wait_for_file(&completed, Duration::from_secs(4)).await,
            "cancelled shell child wrote its delayed completion marker"
        );
        let _ = std::fs::remove_dir_all(base);
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

    #[tokio::test]
    async fn test_run_dry_run_publish_flow_without_command_returns_none() {
        let output = run_dry_run_publish_flow(
            None,
            Path::new("manifest-without-a-parent"),
            &[],
            PACKAGE_DIR_NOT_FOUND,
        )
        .await
        .unwrap();

        assert!(output.is_none());
    }

    #[tokio::test]
    async fn test_run_publish_flow_reports_missing_manifest_parent() {
        let manifest_path = if cfg!(target_os = "windows") {
            Path::new(r"C:\")
        } else {
            Path::new("/")
        };

        let error = run_publish_flow(
            "echo unreachable",
            manifest_path,
            &[],
            WORKSPACE_DIR_NOT_FOUND,
        )
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), WORKSPACE_DIR_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_run_dry_run_publish_flow_forwards_manifest_parent_and_extra_path() {
        let base =
            std::env::temp_dir().join(format!("changepacks_dry_flow_{}", std::process::id()));
        let package_dir = base.join("package");
        let bin_dir = base.join("bin");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();

        let hook = bin_dir.join(if cfg!(target_os = "windows") {
            "cpflowhook.cmd"
        } else {
            "cpflowhook"
        });
        if cfg!(target_os = "windows") {
            std::fs::write(&hook, "@echo %CD%\r\n").unwrap();
        } else {
            std::fs::write(&hook, "#!/bin/sh\npwd\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let output = run_dry_run_publish_flow(
            Some("cpflowhook"),
            &package_dir.join("package.json"),
            std::slice::from_ref(&bin_dir),
            PACKAGE_DIR_NOT_FOUND,
        )
        .await
        .unwrap()
        .unwrap();
        let _ = std::fs::remove_dir_all(&base);

        assert!(output.success, "stderr: {}", output.stderr);
        assert_eq!(
            normalize_path_separators(output.stdout.trim()),
            normalize_path_separators(package_dir.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn test_run_publish_flow_returns_successful_output() {
        let command = if cfg!(target_os = "windows") {
            "echo flow-success"
        } else {
            "printf flow-success"
        };

        let output = run_publish_flow(
            command,
            &std::env::temp_dir().join("package.json"),
            &[],
            PACKAGE_DIR_NOT_FOUND,
        )
        .await
        .unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("flow-success"));
    }

    #[tokio::test]
    async fn test_run_publish_flow_returns_non_zero_exit_status() {
        let command = if cfg!(target_os = "windows") {
            "exit /b 7"
        } else {
            "exit 7"
        };

        let output = run_publish_flow(
            command,
            &std::env::temp_dir().join("package.json"),
            &[],
            PACKAGE_DIR_NOT_FOUND,
        )
        .await
        .unwrap();

        assert!(!output.success);
    }

    #[test]
    fn test_prepend_path_dirs_empty_returns_none() {
        assert!(prepend_path_dirs(&[]).unwrap().is_none());
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
        let joined = prepend_path_dirs(std::slice::from_ref(&dir))
            .expect("valid PATH construction")
            .expect("some PATH");
        let parsed: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(parsed.first(), Some(&dir));
    }

    #[test]
    fn test_prepend_path_dirs_preserves_multi_dir_order_before_existing_path() {
        // `node_modules_bin_dirs_async` hands back a multi-entry `Vec` ordered
        // innermost-package-first, and nearest-wins resolution depends on that
        // order surviving into the child `PATH`. The single-directory test can
        // only pin the first slot, so the relative order of two injected
        // directories — and the fact that the whole process `PATH` still
        // follows them — is asserted here.
        let (first, second) = if cfg!(target_os = "windows") {
            (
                PathBuf::from("C:\\changepacks-path-test-bin-a"),
                PathBuf::from("C:\\changepacks-path-test-bin-b"),
            )
        } else {
            (
                PathBuf::from("/changepacks-path-test-bin-a"),
                PathBuf::from("/changepacks-path-test-bin-b"),
            )
        };

        let joined = prepend_path_dirs(&[first.clone(), second.clone()])
            .expect("valid PATH construction")
            .expect("some PATH");
        let parsed: Vec<PathBuf> = std::env::split_paths(&joined).collect();

        assert_eq!(parsed.first(), Some(&first));
        assert_eq!(parsed.get(1), Some(&second));

        let existing: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        assert_eq!(
            &parsed[2..],
            existing.as_slice(),
            "pre-existing PATH entries must follow the injected directories unchanged"
        );
    }

    #[test]
    fn test_prepend_path_dirs_reports_invalid_platform_separator() {
        #[cfg(target_os = "windows")]
        let invalid_dir = PathBuf::from("C:\\changepacks\"invalid");
        #[cfg(not(target_os = "windows"))]
        let invalid_dir = PathBuf::from("/changepacks:invalid");

        let error = prepend_path_dirs(&[invalid_dir]).unwrap_err();

        assert!(
            error.to_string().contains("failed to construct PATH"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn test_build_shell_command() {
        let cmd = build_shell_command("echo hello");
        let program = cmd.as_std().get_program().to_string_lossy();
        #[cfg(target_os = "windows")]
        assert_eq!(program, "cmd");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(program, "sh");
    }

    #[tokio::test]
    async fn test_run_publish_command_os_args_success() {
        let temp_dir = std::env::temp_dir();
        let output = run_publish_command_os_args("git", ["--version"], &temp_dir, false)
            .await
            .unwrap();
        assert!(output.success, "stderr: {}", output.stderr);
        assert!(
            output.stdout.contains("git version"),
            "unexpected stdout: {}",
            output.stdout
        );
    }

    #[tokio::test]
    async fn test_run_publish_command_os_args_spawn_error() {
        let temp_dir = std::env::temp_dir();
        let result = run_publish_command_os_args(
            "changepacks-no-such-binary-xyz",
            [] as [&str; 0],
            &temp_dir,
            false,
        )
        .await;
        assert!(
            result.is_err(),
            "expected spawn error for nonexistent binary"
        );
    }

    #[tokio::test]
    async fn test_run_publish_command_os_args_non_zero_exit() {
        // The documented contract of `run_publish_command_os_args` is that a
        // non-zero exit code comes back as `Ok(PublishOutput { success: false })`
        // and NOT as an `Err`. The shell-based sibling pins this via
        // `test_run_publish_command_failure`; the argv-based runner backs the
        // whole C# managed dry-run pipeline, so it needs the same guard.
        let temp_dir = std::env::temp_dir();
        let (program, args) = if cfg!(target_os = "windows") {
            ("cmd", ["/C", "exit 1"])
        } else {
            ("sh", ["-c", "exit 1"])
        };
        let output = run_publish_command_os_args(program, args, &temp_dir, false)
            .await
            .expect("non-zero exit must be Ok, not Err");
        assert!(
            !output.success,
            "expected success=false for a non-zero exit, stdout: {}, stderr: {}",
            output.stdout, output.stderr
        );
    }

    #[tokio::test]
    async fn test_run_publish_command_os_args_kill_on_drop_kills_cancelled_child() {
        // `run_publish_command_os_args` documents that with `kill_on_drop =
        // true` a cancelled future terminates the child before the `Child`
        // handle is dropped, but every other argv test passes `false`, so the
        // flag was never exercised. The C# managed dry-run pipeline is the
        // production consumer of the `true` variant, and it relies on this to
        // avoid leaking a `dotnet` child when the publish run is interrupted.
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "changepacks_osargs_cancel_{}_{}",
            std::process::id(),
            unique
        ));
        std::fs::create_dir_all(&base).unwrap();
        let started = base.join("started");
        let completed = base.join("completed");

        // Same cfg-split command shape as the shell-runner cancellation test:
        // write a readiness marker, sleep, then write a delayed completion
        // marker that must never appear once the future is cancelled.
        #[cfg(target_os = "windows")]
        let (program, args) = (
            "cmd",
            [
                "/C",
                "echo started>started & ping -n 3 127.0.0.1 >nul & echo completed>completed",
            ],
        );
        #[cfg(not(target_os = "windows"))]
        let (program, args) = (
            "sh",
            [
                "-c",
                "printf started > started; sleep 2; printf completed > completed",
            ],
        );

        let task_dir = base.clone();
        let mut task = tokio::spawn(async move {
            run_publish_command_os_args(program, args, &task_dir, true).await
        });
        tokio::task::yield_now().await;
        let started_written = wait_for_file(&started, Duration::from_secs(5)).await;
        if !started_written && task.is_finished() {
            panic!(
                "argv command finished before readiness: {:?}",
                (&mut task).await
            );
        }
        assert!(started_written, "argv child never wrote its start marker");

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        // The child writes `completed` after roughly two seconds. Polling for
        // longer than that proves `kill_on_drop` prevented the delayed side
        // effect instead of merely racing the child's startup.
        assert!(
            !wait_for_file(&completed, Duration::from_secs(4)).await,
            "cancelled argv child wrote its delayed completion marker"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_utf8_or_lossy_valid_utf8_passthrough() {
        assert_eq!(utf8_or_lossy("héllo".as_bytes().to_vec()), "héllo");
    }

    #[test]
    fn test_utf8_or_lossy_invalid_utf8_lossy_fallback() {
        assert_eq!(utf8_or_lossy(vec![0x66, 0x6f, 0x80, 0x6f]), "fo\u{FFFD}o");
    }

    #[test]
    fn test_normalize_path_separators_backslash_to_forward_slash() {
        assert_eq!(
            normalize_path_separators("packages\\core\\package.json"),
            "packages/core/package.json"
        );
    }

    #[test]
    fn test_normalize_path_separators_no_backslash_unchanged() {
        assert_eq!(
            normalize_path_separators("packages/core/package.json"),
            "packages/core/package.json"
        );
    }

    #[test]
    fn test_normalize_path_separators_borrows_when_no_backslash() {
        // The allocation-free fast path is the contract, not an accident: a
        // backslash-free path (every path on non-Windows, and an already
        // normalized path on Windows) must come back borrowed. Asserting the
        // `Cow` variant keeps a future refactor from silently reintroducing an
        // unconditional `String` allocation on the hot path.
        let input = "packages/core/package.json";
        let normalized = normalize_path_separators(input);
        assert!(matches!(normalized, Cow::Borrowed(_)));
        assert!(std::ptr::eq(normalized.as_ref(), input));
    }

    #[test]
    fn test_normalize_path_separators_owns_when_backslash_present() {
        let normalized = normalize_path_separators("packages\\core\\package.json");
        assert!(matches!(normalized, Cow::Owned(_)));
        assert_eq!(normalized, "packages/core/package.json");
    }

    #[test]
    fn test_normalize_path_separators_mixed_separators_fully_normalized() {
        // Mixed separators are the realistic Windows shape: a forward-slash
        // `--project` prefix or config-derived segment joined onto a
        // filesystem-derived backslash tail. Every backslash must be rewritten,
        // not just the first, and the pre-existing forward slashes must survive
        // untouched, so the result is comparable against a forward-slash config
        // key. Presence of a single backslash is enough to take the owned path.
        let normalized = normalize_path_separators("packages/core\\src\\lib\\package.json");
        assert!(matches!(normalized, Cow::Owned(_)));
        assert_eq!(normalized, "packages/core/src/lib/package.json");
    }

    #[test]
    fn test_normalize_path_separators_empty_input_borrows_empty() {
        // Reachable in practice: the publish `--project` filter normalizes
        // `Path::new(value).to_string_lossy()`, and an empty relative path
        // (a project at the repository root, or an empty filter value) yields
        // the empty string. It contains no backslash, so it must round-trip
        // unchanged through the allocation-free borrowed branch rather than
        // paying for an empty `String`.
        let normalized = normalize_path_separators("");
        assert!(matches!(normalized, Cow::Borrowed(_)));
        assert_eq!(normalized, "");
    }

    #[test]
    fn test_normalize_path_separators_of_borrows_utf8_path_without_backslash() {
        // A valid-UTF-8, already-normalized path is the overwhelmingly common
        // case (every path on non-Windows). It must borrow straight out of the
        // `Path`, with no `String` allocation anywhere in the chain.
        let path = Path::new("packages/core/package.json");
        let normalized = normalize_path_separators_of(path);
        assert!(matches!(normalized, Cow::Borrowed(_)));
        assert_eq!(normalized, "packages/core/package.json");
    }

    #[test]
    fn test_normalize_path_separators_of_rewrites_utf8_path_with_backslash() {
        // A valid-UTF-8 Windows path pays exactly one allocation for the
        // rewrite, and every backslash is rewritten, not just the first.
        let path = Path::new("packages\\core\\src\\package.json");
        let normalized = normalize_path_separators_of(path);
        assert!(matches!(normalized, Cow::Owned(_)));
        assert_eq!(normalized, "packages/core/src/package.json");
    }

    /// Build a path whose bytes are not valid UTF-8, so `to_string_lossy`
    /// is forced onto its owned, replacement-character branch.
    #[cfg(any(unix, windows))]
    fn non_utf8_path(with_backslash: bool) -> OsString {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let separator = if with_backslash { b'\\' } else { b'/' };
            OsString::from_vec(vec![b'p', 0xFF, separator, b'q'])
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let separator = if with_backslash { 0x005C } else { 0x002F };
            // 0xD800 is an unpaired surrogate: representable in a Windows
            // wide path, but not decodable as UTF-8.
            OsString::from_wide(&[u16::from(b'p'), 0xD800, separator, u16::from(b'q')])
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn test_normalize_path_separators_of_moves_owned_lossy_string_untouched() {
        // A non-UTF-8 path makes `to_string_lossy` allocate before we ever see
        // it. With no backslash there is nothing left to rewrite, so that owned
        // `String` must be moved through as-is rather than copied a second
        // time — the allocation behaviour the old hand-written helper in
        // `gen_update_map` documented and this helper now owns.
        let os_string = non_utf8_path(false);
        let path = Path::new(&os_string);
        let expected = path.to_string_lossy().into_owned();
        let normalized = normalize_path_separators_of(path);
        assert!(matches!(normalized, Cow::Owned(_)));
        assert_eq!(normalized, expected);
        assert!(!normalized.contains('\\'));
        // The lossy replacement character survived the round-trip.
        assert!(normalized.contains('\u{FFFD}'));
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn test_normalize_path_separators_of_rewrites_owned_lossy_string() {
        // The other half of the owned branch: a non-UTF-8 path that DOES carry
        // a backslash still gets fully normalized.
        let os_string = non_utf8_path(true);
        let path = Path::new(&os_string);
        let normalized = normalize_path_separators_of(path);
        assert!(matches!(normalized, Cow::Owned(_)));
        assert!(!normalized.contains('\\'));
        assert_eq!(normalized, format!("p{}/q", '\u{FFFD}'));
    }
}
