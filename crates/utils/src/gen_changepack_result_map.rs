use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{ChangePackResult, ChangePackResultLog, Project, UpdateType};
use gix::hashtable::hash_map::HashMap;

use crate::{get_relative_path, next_version};

pub fn gen_changepack_result_map(
    projects: &[&Project],
    repo_root_path: &Path,
    mut update_result: HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
) -> Result<BTreeMap<PathBuf, ChangePackResult>> {
    let mut map = BTreeMap::<PathBuf, ChangePackResult>::new();
    for project in projects {
        let key = get_relative_path(repo_root_path, project.path())?;
        let result = match update_result.remove(&key) {
            Some((update_type, notes)) => ChangePackResult::new(
                notes,
                project.version().map(|v| v.to_string()),
                Some(next_version(
                    project.version().unwrap_or("0.0.0"),
                    update_type,
                )?),
                project.name().map(|n| n.to_string()),
                project.is_changed(),
            ),
            None => ChangePackResult::new(
                vec![],
                project.version().map(|v| v.to_string()),
                None,
                project.name().map(|n| n.to_string()),
                project.is_changed(),
            ),
        };
        map.insert(key, result);
    }
    Ok(map)
}
