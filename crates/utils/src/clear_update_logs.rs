use std::path::PathBuf;

use anyhow::Result;
use tokio::fs::{read_dir, remove_file};

// remove all update logs without confirmation
pub async fn clear_update_logs(changepacks_dir: &PathBuf) -> Result<()> {
    if !changepacks_dir.exists() {
        return Ok(());
    }
    let mut entries = read_dir(&changepacks_dir).await?;
    let mut update_logs = vec![];
    while let Some(file) = entries.next_entry().await? {
        if file.file_name().to_string_lossy() == "changepacks.json" {
            continue;
        }
        update_logs.push(remove_file(file.path()));
    }

    if futures::future::join_all(update_logs)
        .await
        .iter()
        .all(|f| f.is_ok())
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Failed to remove update logs"))
    }
}

#[cfg(test)]
mod tests {
    use crate::get_changepacks_dir;

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_clear_update_logs_empty_directory() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        // Create .changepacks directory
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Test clearing logs from empty directory
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_clear_update_logs_no_changepacks_directory() {
        // Create a temporary directory without .changepacks
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        // Test clearing logs when .changepacks directory doesn't exist
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());
    }
}
