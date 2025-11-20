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
                key.clone(),
            ),
            None => ChangePackResult::new(
                vec![],
                project.version().map(|v| v.to_string()),
                None,
                project.name().map(|n| n.to_string()),
                project.is_changed(),
                key.clone(),
            ),
        };
        map.insert(key, result);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::Package;
    use changepacks_node::package::NodePackage;
    use serde_json;
    use std::fs;
    use tempfile::TempDir;

    // Helper function to create a mock project
    fn create_test_project(
        name: &str,
        version: &str,
        path: PathBuf,
        relative_path: PathBuf,
        is_changed: bool,
    ) -> Project {
        let mut package = NodePackage::new(
            Some(name.to_string()),
            Some(version.to_string()),
            path,
            relative_path,
        );
        package.set_changed(is_changed);
        Project::Package(Box::new(package))
    }

    // Helper function to extract field from JSON
    fn get_json_field<'a>(
        json: &'a serde_json::Value,
        field: &str,
    ) -> Option<&'a serde_json::Value> {
        json.get(field)
    }

    #[test]
    fn test_gen_changepack_result_map_with_update_result() {
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();

        // Initialize git repo
        std::process::Command::new("git")
            .arg("init")
            .current_dir(repo_root)
            .output()
            .unwrap();

        let project_path = repo_root.join("project1");
        fs::create_dir_all(&project_path).unwrap();
        let package_json = project_path.join("package.json");
        fs::write(
            &package_json,
            r#"{"name": "test-package", "version": "1.0.0"}"#,
        )
        .unwrap();

        let project = create_test_project(
            "test-package",
            "1.0.0",
            package_json.clone(),
            PathBuf::from("project1/package.json"),
            true,
        );

        let mut update_result = HashMap::new();
        let logs = vec![ChangePackResultLog::new(
            UpdateType::Patch,
            "Fixed a bug".to_string(),
        )];
        update_result.insert(
            PathBuf::from("project1/package.json"),
            (UpdateType::Patch, logs),
        );

        let projects = vec![&project];
        let result = gen_changepack_result_map(&projects, repo_root, update_result).unwrap();

        assert_eq!(result.len(), 1);
        let change_result = result.get(&PathBuf::from("project1/package.json")).unwrap();

        // Serialize to JSON to verify fields
        let json = serde_json::to_value(change_result).unwrap();
        assert_eq!(
            get_json_field(&json, "version").and_then(|v| v.as_str()),
            Some("1.0.0")
        );
        assert_eq!(
            get_json_field(&json, "nextVersion").and_then(|v| v.as_str()),
            Some("1.0.1")
        );
        assert_eq!(
            get_json_field(&json, "name").and_then(|v| v.as_str()),
            Some("test-package")
        );
        assert_eq!(
            get_json_field(&json, "changed").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            get_json_field(&json, "logs")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gen_changepack_result_map_without_update_result() {
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();

        // Initialize git repo
        std::process::Command::new("git")
            .arg("init")
            .current_dir(repo_root)
            .output()
            .unwrap();

        let project_path = repo_root.join("project2");
        fs::create_dir_all(&project_path).unwrap();
        let package_json = project_path.join("package.json");
        fs::write(
            &package_json,
            r#"{"name": "test-package-2", "version": "2.5.3"}"#,
        )
        .unwrap();

        let project = create_test_project(
            "test-package-2",
            "2.5.3",
            package_json.clone(),
            PathBuf::from("project2/package.json"),
            false,
        );

        let update_result = HashMap::new();
        let projects = vec![&project];
        let result = gen_changepack_result_map(&projects, repo_root, update_result).unwrap();

        assert_eq!(result.len(), 1);
        let change_result = result.get(&PathBuf::from("project2/package.json")).unwrap();

        // Serialize to JSON to verify fields
        let json = serde_json::to_value(change_result).unwrap();
        assert_eq!(
            get_json_field(&json, "version").and_then(|v| v.as_str()),
            Some("2.5.3")
        );
        assert!(
            get_json_field(&json, "nextVersion").is_none()
                || get_json_field(&json, "nextVersion").unwrap().is_null()
        );
        assert_eq!(
            get_json_field(&json, "name").and_then(|v| v.as_str()),
            Some("test-package-2")
        );
        assert_eq!(
            get_json_field(&json, "changed").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            get_json_field(&json, "logs")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(0)
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gen_changepack_result_map_multiple_projects() {
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();

        // Initialize git repo
        std::process::Command::new("git")
            .arg("init")
            .current_dir(repo_root)
            .output()
            .unwrap();

        // Create first project
        let project1_path = repo_root.join("project1");
        fs::create_dir_all(&project1_path).unwrap();
        let package_json1 = project1_path.join("package.json");
        fs::write(
            &package_json1,
            r#"{"name": "package1", "version": "1.0.0"}"#,
        )
        .unwrap();

        let project1 = create_test_project(
            "package1",
            "1.0.0",
            package_json1.clone(),
            PathBuf::from("project1/package.json"),
            true,
        );

        // Create second project
        let project2_path = repo_root.join("project2");
        fs::create_dir_all(&project2_path).unwrap();
        let package_json2 = project2_path.join("package.json");
        fs::write(
            &package_json2,
            r#"{"name": "package2", "version": "2.0.0"}"#,
        )
        .unwrap();

        let project2 = create_test_project(
            "package2",
            "2.0.0",
            package_json2.clone(),
            PathBuf::from("project2/package.json"),
            false,
        );

        let mut update_result = HashMap::new();
        update_result.insert(
            PathBuf::from("project1/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Added new feature".to_string(),
                )],
            ),
        );
        // project2 has no update result

        let projects = vec![&project1, &project2];
        let result = gen_changepack_result_map(&projects, repo_root, update_result).unwrap();

        assert_eq!(result.len(), 2);

        let result1 = result.get(&PathBuf::from("project1/package.json")).unwrap();
        let json1 = serde_json::to_value(result1).unwrap();
        assert_eq!(
            get_json_field(&json1, "version").and_then(|v| v.as_str()),
            Some("1.0.0")
        );
        assert_eq!(
            get_json_field(&json1, "nextVersion").and_then(|v| v.as_str()),
            Some("1.1.0")
        );
        assert_eq!(
            get_json_field(&json1, "logs")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );

        let result2 = result.get(&PathBuf::from("project2/package.json")).unwrap();
        let json2 = serde_json::to_value(result2).unwrap();
        assert_eq!(
            get_json_field(&json2, "version").and_then(|v| v.as_str()),
            Some("2.0.0")
        );
        assert!(
            get_json_field(&json2, "nextVersion").is_none()
                || get_json_field(&json2, "nextVersion").unwrap().is_null()
        );
        assert_eq!(
            get_json_field(&json2, "logs")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(0)
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gen_changepack_result_map_major_update() {
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();

        // Initialize git repo
        std::process::Command::new("git")
            .arg("init")
            .current_dir(repo_root)
            .output()
            .unwrap();

        let project_path = repo_root.join("project3");
        fs::create_dir_all(&project_path).unwrap();
        let package_json = project_path.join("package.json");
        fs::write(
            &package_json,
            r#"{"name": "test-package", "version": "1.2.3"}"#,
        )
        .unwrap();

        let project = create_test_project(
            "test-package",
            "1.2.3",
            package_json.clone(),
            PathBuf::from("project3/package.json"),
            true,
        );

        let mut update_result = HashMap::new();
        update_result.insert(
            PathBuf::from("project3/package.json"),
            (
                UpdateType::Major,
                vec![ChangePackResultLog::new(
                    UpdateType::Major,
                    "Breaking change".to_string(),
                )],
            ),
        );

        let projects = vec![&project];
        let result = gen_changepack_result_map(&projects, repo_root, update_result).unwrap();

        let change_result = result.get(&PathBuf::from("project3/package.json")).unwrap();
        let json = serde_json::to_value(change_result).unwrap();
        assert_eq!(
            get_json_field(&json, "version").and_then(|v| v.as_str()),
            Some("1.2.3")
        );
        assert_eq!(
            get_json_field(&json, "nextVersion").and_then(|v| v.as_str()),
            Some("2.0.0")
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gen_changepack_result_map_project_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();

        // Initialize git repo
        std::process::Command::new("git")
            .arg("init")
            .current_dir(repo_root)
            .output()
            .unwrap();

        let project_path = repo_root.join("project4");
        fs::create_dir_all(&project_path).unwrap();
        let package_json = project_path.join("package.json");
        fs::write(&package_json, r#"{"name": "test-package"}"#).unwrap();

        // Create a project without version - use "0.0.0" as default
        // The function uses "0.0.0" as default when version is None
        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("0.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("project4/package.json"),
        );
        package.set_changed(false);
        let project = Project::Package(Box::new(package));

        let mut update_result = HashMap::new();
        update_result.insert(
            PathBuf::from("project4/package.json"),
            (
                UpdateType::Patch,
                vec![ChangePackResultLog::new(
                    UpdateType::Patch,
                    "Initial release".to_string(),
                )],
            ),
        );

        let projects = vec![&project];
        let result = gen_changepack_result_map(&projects, repo_root, update_result).unwrap();

        let change_result = result.get(&PathBuf::from("project4/package.json")).unwrap();
        let json = serde_json::to_value(change_result).unwrap();
        // When version is "0.0.0", next_version should be "0.0.1" for Patch update
        assert_eq!(
            get_json_field(&json, "version").and_then(|v| v.as_str()),
            Some("0.0.0")
        );
        assert_eq!(
            get_json_field(&json, "nextVersion").and_then(|v| v.as_str()),
            Some("0.0.1")
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gen_changepack_result_map_multiple_logs() {
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();

        // Initialize git repo
        std::process::Command::new("git")
            .arg("init")
            .current_dir(repo_root)
            .output()
            .unwrap();

        let project_path = repo_root.join("project5");
        fs::create_dir_all(&project_path).unwrap();
        let package_json = project_path.join("package.json");
        fs::write(
            &package_json,
            r#"{"name": "test-package", "version": "1.0.0"}"#,
        )
        .unwrap();

        let project = create_test_project(
            "test-package",
            "1.0.0",
            package_json.clone(),
            PathBuf::from("project5/package.json"),
            true,
        );

        let mut update_result = HashMap::new();
        update_result.insert(
            PathBuf::from("project5/package.json"),
            (
                UpdateType::Minor,
                vec![
                    ChangePackResultLog::new(UpdateType::Minor, "Added feature A".to_string()),
                    ChangePackResultLog::new(UpdateType::Minor, "Added feature B".to_string()),
                    ChangePackResultLog::new(UpdateType::Minor, "Improved performance".to_string()),
                ],
            ),
        );

        let projects = vec![&project];
        let result = gen_changepack_result_map(&projects, repo_root, update_result).unwrap();

        let change_result = result.get(&PathBuf::from("project5/package.json")).unwrap();
        let json = serde_json::to_value(change_result).unwrap();
        assert_eq!(
            get_json_field(&json, "logs")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(3)
        );

        temp_dir.close().unwrap();
    }
}
