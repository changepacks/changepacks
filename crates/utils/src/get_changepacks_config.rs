use std::path::Path;

use anyhow::{Context, Result};
use changepacks_core::Config;
use tokio::fs::read_to_string;

use crate::get_changepacks_dir;

/// Get the changepacks configuration from .changepacks/config.json
/// Returns default config if the file doesn't exist or is empty
pub async fn get_changepacks_config(current_dir: &Path) -> Result<Config> {
    let changepacks_dir = get_changepacks_dir(current_dir)?;
    let config_file = changepacks_dir.join("config.json");

    if !config_file.exists() {
        return Ok(Config::default());
    }

    let content = read_to_string(&config_file).await?;

    // If file is empty or only whitespace, return default config
    if content.trim().is_empty() {
        return Ok(Config::default());
    }

    // Parse JSON config, merging with defaults
    let config: Config = serde_json::from_str(&content).context("Failed to parse config.json")?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use tokio::fs::write;

    #[tokio::test]
    async fn test_get_changepacks_config_default() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        let config = get_changepacks_config(temp_path).await.unwrap();
        assert_eq!(config.ignore, Vec::<String>::new());
        assert_eq!(config.base_branch, "main");

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_changepacks_config_from_file() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();
        let config_file = changepacks_dir.join("config.json");

        let config_json = r#"{
            "ignore": ["node_modules", "target"],
            "baseBranch": "develop"
        }"#;
        write(&config_file, config_json).await.unwrap();

        let config = get_changepacks_config(temp_path).await.unwrap();
        assert_eq!(config.ignore, vec!["node_modules", "target"]);
        assert_eq!(config.base_branch, "develop");

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_changepacks_config_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();
        let config_file = changepacks_dir.join("config.json");

        // Write empty file
        write(&config_file, "{}").await.unwrap();

        let config = get_changepacks_config(temp_path).await.unwrap();
        assert_eq!(config.ignore, Vec::<String>::new());
        assert_eq!(config.base_branch, "main");

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_changepacks_config_partial_config() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();
        let config_file = changepacks_dir.join("config.json");

        // Only specify ignore, baseBranch should default
        let config_json = r#"{
            "ignore": ["dist"]
        }"#;
        write(&config_file, config_json).await.unwrap();

        let config = get_changepacks_config(temp_path).await.unwrap();
        assert_eq!(config.ignore, vec!["dist"]);
        assert_eq!(config.base_branch, "main"); // Should use default

        temp_dir.close().unwrap();
    }
}
