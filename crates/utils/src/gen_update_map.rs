use std::path::{Path, PathBuf};

use anyhow::Result;
use changepacks_core::{ChangePackLog, update_type::UpdateType};
use gix::hashtable::hash_map::HashMap;
use tokio::fs::{read_dir, read_to_string};

use crate::get_changepacks_dir;

pub async fn gen_update_map(current_dir: &Path) -> Result<HashMap<PathBuf, UpdateType>> {
    let mut update_map = HashMap::<PathBuf, UpdateType>::new();
    let changepacks_dir = get_changepacks_dir(current_dir)?;

    let mut entries = read_dir(&changepacks_dir).await?;
    while let Some(file) = entries.next_entry().await? {
        let file_name = file.file_name().to_string_lossy().to_string();
        if file_name == "changepacks.json" || !file_name.ends_with(".json") {
            continue;
        }
        let file_json = read_to_string(file.path()).await?;
        let file_json: ChangePackLog = serde_json::from_str(&file_json)?;
        for (project_path, update_type) in file_json.changes().iter() {
            if update_map.contains_key(project_path) {
                if update_map[project_path] < *update_type {
                    update_map.insert(project_path.clone(), update_type.clone());
                }
                continue;
            }
            update_map.insert(project_path.clone(), update_type.clone());
        }
    }
    Ok(update_map)
}
