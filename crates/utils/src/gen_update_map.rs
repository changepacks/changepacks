use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{ChangePackLog, ChangePackResultLog, UpdateType};
use tokio::fs::{read_dir, read_to_string};

use crate::get_changepacks_dir;

pub async fn gen_update_map(
    current_dir: &Path,
) -> Result<HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>> {
    let mut update_map = HashMap::<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>::new();
    let changepacks_dir = get_changepacks_dir(current_dir)?;

    let mut entries = read_dir(&changepacks_dir).await?;
    while let Some(file) = entries.next_entry().await? {
        let file_name = file.file_name().to_string_lossy().to_string();
        if file_name == "config.json" || !file_name.ends_with(".json") {
            continue;
        }
        let file_json = read_to_string(file.path()).await?;
        let file_json: ChangePackLog = serde_json::from_str(&file_json)?;
        for (project_path, update_type) in file_json.changes().iter() {
            let ret = update_map
                .entry(project_path.clone())
                .or_insert((update_type.clone(), vec![]));
            ret.1.push(ChangePackResultLog::new(
                update_type.clone(),
                file_json.note().to_string(),
            ));
            if ret.0 > *update_type {
                ret.0 = update_type.clone();
            }
        }
    }
    Ok(update_map)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use changepacks_core::Config;
    use tempfile::TempDir;
    use tokio::fs;

    use super::*;

    #[tokio::test]
    async fn test_gen_update_map() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();
        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).await.unwrap();

        {
            assert!(gen_update_map(&temp_path).await.unwrap().is_empty());
        }
        {
            fs::write(
                changepacks_dir.join("config.json"),
                serde_json::to_string(&Config::default()).unwrap(),
            )
            .await
            .unwrap();
            assert!(gen_update_map(&temp_path).await.unwrap().is_empty());
        }
        {
            fs::write(changepacks_dir.join("wrong.file"), "{}")
                .await
                .unwrap();
            assert!(gen_update_map(&temp_path).await.unwrap().is_empty());
        }
        {
            let mut map = HashMap::new();
            map.insert(temp_path.join("package"), UpdateType::Patch);
            let changepack_log = ChangePackLog::new(map, "".to_string());

            fs::write(
                changepacks_dir.join("changepack_log_1.json"),
                serde_json::to_string(&changepack_log).unwrap(),
            )
            .await
            .unwrap();
            let update_map = gen_update_map(&temp_path).await.unwrap();
            assert!(update_map.len() == 1);
            assert!(update_map.contains_key(&temp_path.join("package")));
            assert!(update_map[&temp_path.join("package")].0 == UpdateType::Patch);
        }

        {
            let update_map = gen_update_map(&temp_path).await.unwrap();
            assert!(update_map.len() == 1);

            let mut map = HashMap::new();
            map.insert(temp_path.join("package"), UpdateType::Minor);
            let changepack_log = ChangePackLog::new(map, "".to_string());

            fs::write(
                changepacks_dir.join("changepack_log_2.json"),
                serde_json::to_string(&changepack_log).unwrap(),
            )
            .await
            .unwrap();
            let update_map = gen_update_map(&temp_path).await.unwrap();
            assert!(update_map.len() == 1);
            assert!(update_map.contains_key(&temp_path.join("package")));
            // overwrite the previous update type
            assert!(update_map[&temp_path.join("package")].0 == UpdateType::Minor);
        }
        {
            let mut map = HashMap::new();
            map.insert(temp_path.join("package2"), UpdateType::Major);
            let changepack_log = ChangePackLog::new(map, "".to_string());

            fs::write(
                changepacks_dir.join("changepack_log_3.json"),
                serde_json::to_string(&changepack_log).unwrap(),
            )
            .await
            .unwrap();
            let update_map = gen_update_map(&temp_path).await.unwrap();
            assert!(update_map.len() == 2);
            assert!(update_map.contains_key(&temp_path.join("package2")));
            assert!(update_map[&temp_path.join("package2")].0 == UpdateType::Major);
        }
        {
            let mut map = HashMap::new();
            map.insert(temp_path.join("package2"), UpdateType::Patch);
            let changepack_log = ChangePackLog::new(map, "".to_string());

            fs::write(
                changepacks_dir.join("changepack_log_4.json"),
                serde_json::to_string(&changepack_log).unwrap(),
            )
            .await
            .unwrap();
            let update_map = gen_update_map(&temp_path).await.unwrap();
            assert!(update_map.len() == 2);
            assert!(update_map.contains_key(&temp_path.join("package2")));
            // remain
            assert!(update_map[&temp_path.join("package2")].0 == UpdateType::Major);
        }
        temp_dir.close().unwrap();
    }
}
