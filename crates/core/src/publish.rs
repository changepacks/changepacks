use crate::{Config, Language};
use anyhow::Result;
use std::path::Path;

/// Resolve the publish command from config, language, or default
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

/// Execute a publish command in the given directory
pub async fn run_publish_command(command: &str, working_dir: &Path) -> Result<()> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(working_dir);
    let output = cmd.output().await?;
    if !output.status.success() {
        anyhow::bail!(
            "Publish command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
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

    #[tokio::test]
    async fn test_run_publish_command_success() {
        let temp_dir = std::env::temp_dir();
        let command = if cfg!(target_os = "windows") {
            "cmd /c echo publish"
        } else {
            "echo publish"
        };
        let result = run_publish_command(command, &temp_dir).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_publish_command_failure() {
        let temp_dir = std::env::temp_dir();
        let command = if cfg!(target_os = "windows") {
            "cmd /c exit 1"
        } else {
            "exit 1"
        };
        let result = run_publish_command(command, &temp_dir).await;
        assert!(result.is_err());
    }
}
