use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{ChangePackLog, ChangePackResultLog, Config, UpdateType};
use glob::Pattern;
use tokio::fs::{read_dir, read_to_string};

use crate::get_changepacks_dir;

pub async fn gen_update_map(
    current_dir: &Path,
    config: &Config,
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

    // Apply updateOn rules: if any updated package matches a trigger pattern,
    // add dependent packages as PATCH updates
    apply_update_on_rules(&mut update_map, config);

    Ok(update_map)
}

fn apply_update_on_rules(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    config: &Config,
) {
    let updated_paths: Vec<PathBuf> = update_map.keys().cloned().collect();

    for (trigger_pattern, dependents) in &config.update_on {
        let pattern = match Pattern::new(trigger_pattern) {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Check if any updated package matches the trigger pattern
        let has_trigger = updated_paths.iter().any(|path| {
            let path_str = path.to_string_lossy();
            pattern.matches(&path_str)
        });

        if has_trigger {
            // Add dependent packages as PATCH updates if not already in update_map
            for dependent in dependents {
                let dependent_path = PathBuf::from(dependent);
                update_map.entry(dependent_path).or_insert_with(|| {
                    (
                        UpdateType::Patch,
                        vec![ChangePackResultLog::new(
                            UpdateType::Patch,
                            format!(
                                "Auto-update triggered by updateOn rule: {}",
                                trigger_pattern
                            ),
                        )],
                    )
                });
            }
        }
    }
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
        let config = Config::default();

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
            assert!(
                gen_update_map(&temp_path, &config)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        {
            fs::write(
                changepacks_dir.join("config.json"),
                serde_json::to_string(&Config::default()).unwrap(),
            )
            .await
            .unwrap();
            assert!(
                gen_update_map(&temp_path, &config)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        {
            fs::write(changepacks_dir.join("wrong.file"), "{}")
                .await
                .unwrap();
            assert!(
                gen_update_map(&temp_path, &config)
                    .await
                    .unwrap()
                    .is_empty()
            );
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
            let update_map = gen_update_map(&temp_path, &config).await.unwrap();
            assert!(update_map.len() == 1);
            assert!(update_map.contains_key(&temp_path.join("package")));
            assert!(update_map[&temp_path.join("package")].0 == UpdateType::Patch);
        }

        {
            let update_map = gen_update_map(&temp_path, &config).await.unwrap();
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
            let update_map = gen_update_map(&temp_path, &config).await.unwrap();
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
            let update_map = gen_update_map(&temp_path, &config).await.unwrap();
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
            let update_map = gen_update_map(&temp_path, &config).await.unwrap();
            assert!(update_map.len() == 2);
            assert!(update_map.contains_key(&temp_path.join("package2")));
            // remain
            assert!(update_map[&temp_path.join("package2")].0 == UpdateType::Major);
        }
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_on_rules() {
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

        // Create config with updateOn rule
        let mut update_on = HashMap::new();
        update_on.insert(
            "crates/*".to_string(),
            vec!["bridge/node".to_string(), "bridge/python".to_string()],
        );
        let config = Config {
            update_on,
            ..Default::default()
        };

        // Create a changepack log for crates/core
        let mut map = HashMap::new();
        map.insert(PathBuf::from("crates/core"), UpdateType::Minor);
        let changepack_log = ChangePackLog::new(map, "Update core".to_string());

        fs::write(
            changepacks_dir.join("changepack_log.json"),
            serde_json::to_string(&changepack_log).unwrap(),
        )
        .await
        .unwrap();

        let update_map = gen_update_map(&temp_path, &config).await.unwrap();

        // Should have 3 entries: crates/core (Minor), bridge/node (Patch), bridge/python (Patch)
        assert_eq!(update_map.len(), 3);
        assert!(update_map.contains_key(&PathBuf::from("crates/core")));
        assert!(update_map.contains_key(&PathBuf::from("bridge/node")));
        assert!(update_map.contains_key(&PathBuf::from("bridge/python")));

        // Original update should remain Minor
        assert_eq!(
            update_map[&PathBuf::from("crates/core")].0,
            UpdateType::Minor
        );
        // Dependent updates should be Patch
        assert_eq!(
            update_map[&PathBuf::from("bridge/node")].0,
            UpdateType::Patch
        );
        assert_eq!(
            update_map[&PathBuf::from("bridge/python")].0,
            UpdateType::Patch
        );

        temp_dir.close().unwrap();
    }
}
