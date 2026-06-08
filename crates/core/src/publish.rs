use crate::{Config, Language};
use anyhow::Result;
use std::path::Path;

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

/// Resolve the publish command from config, language, or default
#[must_use]
pub fn resolve_publish_command(
    relative_path: &Path,
    language: Language,
    default_command: &str,
    config: &Config,
) -> String {
    // Check by relative path
    if let Some(cmd) = config.publish.get(relative_path.to_string_lossy().as_ref()) {
        return cmd.clone();
    }
    // Check by language
    let lang_key = language.publish_key();
    if let Some(cmd) = config.publish.get(lang_key) {
        return cmd.clone();
    }
    default_command.to_string()
}

/// Resolve the dry-run publish command from config or fall back to the
/// language crate's `default_dry_run_command`.
///
/// Returns `None` when the language has no built-in dry-run command and the
/// user has not provided an override in `config.publish_dry_run`. Callers
/// should treat `None` as "dry-run not supported for this project; skip with a
/// warning" rather than as a failure.
#[must_use]
pub fn resolve_dry_run_publish_command(
    relative_path: &Path,
    language: Language,
    default_dry_run_command: Option<&str>,
    config: &Config,
) -> Option<String> {
    // 1) Per-project override
    if let Some(cmd) = config
        .publish_dry_run
        .get(relative_path.to_string_lossy().as_ref())
    {
        return Some(cmd.clone());
    }
    // 2) Per-language override
    if let Some(cmd) = config.publish_dry_run.get(language.publish_key()) {
        return Some(cmd.clone());
    }
    // 3) Fall back to the language crate's own default dry-run command
    default_dry_run_command.map(str::to_string)
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

/// Execute a publish command in the given directory and return captured output.
///
/// # Errors
/// Returns error if the command fails to spawn (e.g., binary not found).
/// A non-zero exit code is reported via `PublishOutput::success = false`, not as an error.
pub async fn run_publish_command(command: &str, working_dir: &Path) -> Result<PublishOutput> {
    let mut cmd = build_shell_command(command);
    cmd.current_dir(working_dir);
    let output = cmd.output().await?;
    // Note: from_utf8_lossy silently replaces invalid UTF-8 with replacement characters.
    // This is acceptable since child processes may produce non-UTF-8 bytes.
    Ok(PublishOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
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
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).current_dir(working_dir);
    cmd.kill_on_drop(kill_on_drop);
    let output = cmd.output().await?;
    Ok(PublishOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
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
            "npm publish",
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
            "npm publish",
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
            "npm publish",
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
            Some("npm publish --dry-run"),
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
            Some("npm publish --dry-run"),
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
            Some("npm publish --dry-run"),
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
            None,
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
            None,
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
            None,
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
