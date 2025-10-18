use anyhow::Result;
use changepack_core::{UpdateLog, update_type::UpdateType};
use gix::hashtable::hash_map::HashMap;
use tokio::fs::{read_dir, read_to_string};

use crate::get_changepack_dir;

pub async fn gen_update_map() -> Result<HashMap<String, UpdateType>> {
    let mut update_map = HashMap::<String, UpdateType>::new();
    let changepack_dir = get_changepack_dir()?;

    let mut entries = read_dir(&changepack_dir).await?;
    while let Some(file) = entries.next_entry().await? {
        if file.file_name().to_string_lossy() == "changepack.json" {
            continue;
        }
        let file_json = read_to_string(file.path()).await?;
        let file_json: UpdateLog = serde_json::from_str(&file_json)?;
        for (project_path, update_type) in file_json.changes().iter() {
            if update_map.contains_key(project_path) {
                if update_map[project_path] < *update_type {
                    update_map.insert(project_path.to_string(), update_type.clone());
                }
                continue;
            }
            update_map.insert(project_path.to_string(), update_type.clone());
        }
    }
    Ok(update_map)
}
