use std::path::PathBuf;

use anyhow::Result;
use tokio::fs::{read_dir, remove_file};

// remove all update logs without confirmation
pub async fn clear_update_logs(changepack_dir: &PathBuf) -> Result<()> {
    if !changepack_dir.exists() {
        return Ok(());
    }
    let mut entries = read_dir(&changepack_dir).await?;
    let mut update_logs = vec![];
    while let Some(file) = entries.next_entry().await? {
        if file.file_name().to_string_lossy() == "changepack.json" {
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
    use crate::get_changepack_dir;

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

        // Create .changepack directory
        let changepack_dir = get_changepack_dir(temp_path).unwrap();
        fs::create_dir_all(&changepack_dir).unwrap();

        // Test clearing logs from empty directory
        let result = clear_update_logs(&changepack_dir).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_clear_update_logs_no_changepack_directory() {
        // Create a temporary directory without .changepack
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        // Test clearing logs when .changepack directory doesn't exist
        let changepack_dir = get_changepack_dir(temp_path).unwrap();
        let result = clear_update_logs(&changepack_dir).await;
        assert!(result.is_ok());
    }
}
