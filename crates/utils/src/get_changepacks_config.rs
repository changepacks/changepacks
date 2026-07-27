use std::path::Path;

use anyhow::{Context, Result};
use changepacks_core::Config;
use tokio::fs::read_to_string;

use crate::get_changepacks_dir;

/// Get the changepacks configuration from `<changepacks_dir>/config.json`.
///
/// Same body as [`get_changepacks_config`] but takes an already-computed
/// `.changepacks/` directory so callers that already hold the repo root (e.g.
/// `CommandContext::new`, which caches `repo.work_dir().join(".changepacks")`)
/// can skip the second `gix::discover(current_dir)` walk that
/// [`get_changepacks_config`] performs via [`get_changepacks_dir`].
///
/// Returns the default config if the file doesn't exist or is empty (same
/// behaviour as the current-dir wrapper).
///
/// # Errors
/// Returns error if reading or parsing the config.json file fails.
pub async fn get_changepacks_config_at(changepacks_dir: &Path) -> Result<Config> {
    let config_file = changepacks_dir.join("config.json");

    let content = match read_to_string(&config_file).await {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to read changepacks config {}",
                    config_file.display()
                )
            });
        }
    };

    // If file is empty or only whitespace, return default config
    if content.trim().is_empty() {
        return Ok(Config::default());
    }

    // Parse JSON config, merging with defaults
    let config: Config = serde_json::from_str(&content).with_context(|| {
        format!(
            "Failed to parse changepacks config {}",
            config_file.display()
        )
    })?;

    Ok(config)
}

/// Get the changepacks configuration from .changepacks/config.json
/// Returns default config if the file doesn't exist or is empty
///
/// # Errors
/// Returns error if reading or parsing the config.json file fails.
pub async fn get_changepacks_config(current_dir: &Path) -> Result<Config> {
    let changepacks_dir = get_changepacks_dir(current_dir)?;
    get_changepacks_config_at(&changepacks_dir).await
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

        crate::test_support::init_git_repo(temp_path);

        let config = get_changepacks_config(temp_path).await.unwrap();
        assert_eq!(config.ignore, Vec::<String>::new());
        assert_eq!(config.base_branch, "main");

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_changepacks_config_from_file() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        crate::test_support::init_git_repo(temp_path);

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

        crate::test_support::init_git_repo(temp_path);

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

        crate::test_support::init_git_repo(temp_path);

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

    #[tokio::test]
    async fn test_get_changepacks_config_empty_json() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();
        let config_file = changepacks_dir.join("config.json");

        write(&config_file, "").await.unwrap();

        let config = get_changepacks_config(temp_path).await.unwrap();
        assert_eq!(config, Config::default());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_changepacks_config_malformed_json_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();
        let config_file = changepacks_dir.join("config.json");

        write(&config_file, "{").await.unwrap();

        let err = get_changepacks_config(temp_path).await.unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains(".changepacks"));
        assert!(rendered.contains("config.json"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_changepacks_config_unreadable_file_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Create config.json as a directory so reading it yields a non-NotFound
        // io error on both Windows and Unix.
        let config_file = changepacks_dir.join("config.json");
        fs::create_dir_all(&config_file).unwrap();

        let err = get_changepacks_config_at(&changepacks_dir)
            .await
            .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("Failed to read changepacks config"),
            "unexpected error: {rendered}"
        );
        assert!(
            rendered.contains(&config_file.display().to_string()),
            "unexpected error: {rendered}"
        );

        temp_dir.close().unwrap();
    }
}
