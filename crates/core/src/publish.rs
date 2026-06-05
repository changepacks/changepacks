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

/// Resolve the dry-run publish command from config or by deriving it from the
/// regular publish command plus the language's `dry_run_flag`.
///
/// Returns `None` when the language has no built-in dry-run flag and the user
/// has not provided an override in `config.publish_dry_run`. Callers should
/// treat `None` as "dry-run not supported for this project; skip with a
/// warning" rather than as a failure.
#[must_use]
pub fn resolve_dry_run_publish_command(
    relative_path: &Path,
    language: Language,
    default_command: &str,
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
    // 3) Derive from the regular publish command + language's dry-run flag
    let flag = language.dry_run_flag()?;
    let base = resolve_publish_command(relative_path, language, default_command, config);
    Some(format!("{base} {flag}"))
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
            "npm publish",
            &config,
        );
        assert_eq!(result.as_deref(), Some("custom dry"));
    }

    #[test]
    fn test_resolve_dry_run_publish_command_by_language() {
        let mut publish_dry_run = HashMap::new();
        publish_dry_run.insert("node".to_string(), "npm publish --dry-run -tag".to_string());
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        let result = resolve_dry_run_publish_command(
            Path::new("package.json"),
            Language::Node,
            "npm publish",
            &config,
        );
        assert_eq!(result.as_deref(), Some("npm publish --dry-run -tag"));
    }

    #[test]
    fn test_resolve_dry_run_publish_command_derived_from_publish() {
        let config = Config::default();

        let result = resolve_dry_run_publish_command(
            Path::new("package.json"),
            Language::Node,
            "npm publish",
            &config,
        );
        assert_eq!(result.as_deref(), Some("npm publish --dry-run"));
    }

    #[test]
    fn test_resolve_dry_run_publish_command_uses_config_publish_then_appends_flag() {
        // When config provides a custom `publish` command but no dry-run override,
        // the dry-run command should append the language flag to the custom command.
        let mut publish = HashMap::new();
        publish.insert(
            "node".to_string(),
            "npm publish --access public".to_string(),
        );
        let config = Config {
            publish,
            ..Default::default()
        };

        let result = resolve_dry_run_publish_command(
            Path::new("package.json"),
            Language::Node,
            "npm publish",
            &config,
        );
        assert_eq!(
            result.as_deref(),
            Some("npm publish --access public --dry-run")
        );
    }

    #[test]
    fn test_resolve_dry_run_publish_command_unsupported_language_returns_none() {
        let config = Config::default();

        let result = resolve_dry_run_publish_command(
            Path::new("project.csproj"),
            Language::CSharp,
            "dotnet nuget push",
            &config,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_dry_run_publish_command_unsupported_with_override() {
        let mut publish_dry_run = HashMap::new();
        publish_dry_run.insert("csharp".to_string(), "dotnet pack -c Release".to_string());
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        let result = resolve_dry_run_publish_command(
            Path::new("project.csproj"),
            Language::CSharp,
            "dotnet nuget push",
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
