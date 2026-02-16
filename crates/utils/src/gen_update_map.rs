use std::{
    collections::{HashMap, HashSet},
    hash::BuildHasher,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{ChangePackLog, ChangePackResultLog, Config, Project, UpdateType};
use glob::Pattern;
use tokio::fs::{read_dir, read_to_string};

use crate::get_changepacks_dir;

/// Generate update map from changepack logs
///
/// # Errors
/// Returns error if reading changepacks directory or parsing JSON fails.
pub async fn gen_update_map(
    current_dir: &Path,
    config: &Config,
) -> Result<HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>> {
    let mut update_map = HashMap::<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>::new();
    let changepacks_dir = get_changepacks_dir(current_dir)?;

    let mut entries = read_dir(&changepacks_dir).await?;
    while let Some(file) = entries.next_entry().await? {
        let file_name = file.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.as_ref() == "config.json"
            || !Path::new(file_name.as_ref())
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        let file_json = read_to_string(file.path()).await?;
        let file_json: ChangePackLog = serde_json::from_str(&file_json)?;
        for (project_path, update_type) in file_json.changes() {
            let ret = update_map
                .entry(project_path.clone())
                .or_insert((*update_type, vec![]));
            ret.1.push(ChangePackResultLog::new(
                *update_type,
                file_json.note().to_string(),
            ));
            if ret.0 > *update_type {
                ret.0 = *update_type;
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
        let Ok(pattern) = Pattern::new(trigger_pattern) else {
            eprintln!("warning: invalid glob pattern in updateOn config: {trigger_pattern}");
            continue;
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
                            format!("Auto-update triggered by updateOn rule: {trigger_pattern}"),
                        )],
                    )
                });
            }
        }
    }
}

/// Apply reverse dependency updates: if package A depends on package B (via workspace:*),
/// and B is being updated, then A should also be updated as PATCH.
pub fn apply_reverse_dependencies<S: BuildHasher>(
    update_map: &mut HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>), S>,
    projects: &[&Project],
    repo_root_path: &Path,
) {
    // Build a map from package name to its relative file path (e.g., "crates/core/Cargo.toml")
    let mut name_to_path: HashMap<String, PathBuf> = HashMap::new();
    for project in projects {
        if let Some(name) = project.name()
            && let Ok(rel_path) = project.path().strip_prefix(repo_root_path)
        {
            name_to_path.insert(name.to_string(), rel_path.to_path_buf());
        }
    }

    // Build reverse dependency map: updated_package_name -> [packages that depend on it]
    let mut reverse_deps: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
    for project in projects {
        let dependencies = project.dependencies();
        if !dependencies.is_empty()
            && let Ok(rel_path) = project.path().strip_prefix(repo_root_path)
        {
            let project_path = rel_path.to_path_buf();
            let project_name = project.name().unwrap_or("unknown").to_string();

            for dep_name in dependencies {
                reverse_deps
                    .entry(dep_name.clone())
                    .or_default()
                    .push((project_path.clone(), project_name.clone()));
            }
        }
    }

    // Find all packages that need to be updated due to dependencies
    let mut packages_to_add: Vec<(PathBuf, String)> = Vec::new();
    let mut processed: HashSet<PathBuf> = HashSet::new();

    // Initial set of updated package names
    let updated_names: HashSet<String> = update_map
        .keys()
        .filter_map(|path| {
            // Find the package name for this path
            name_to_path.iter().find_map(
                |(name, p)| {
                    if p == path { Some(name.clone()) } else { None }
                },
            )
        })
        .collect();

    // Process reverse dependencies transitively
    let mut to_process: Vec<String> = updated_names.into_iter().collect();
    while let Some(pkg_name) = to_process.pop() {
        if let Some(dependents) = reverse_deps.get(&pkg_name) {
            for (dep_path, dep_name) in dependents {
                if !processed.contains(dep_path) && !update_map.contains_key(dep_path) {
                    processed.insert(dep_path.clone());
                    packages_to_add.push((dep_path.clone(), pkg_name.clone()));
                    to_process.push(dep_name.clone());
                }
            }
        }
    }

    // Add the dependent packages to update_map
    for (path, dependency_name) in packages_to_add {
        update_map.entry(path).or_insert_with(|| {
            (
                UpdateType::Patch,
                vec![ChangePackResultLog::new(
                    UpdateType::Patch,
                    format!("Auto-update: depends on '{dependency_name}' via workspace:*"),
                )],
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use changepacks_core::{Config, Package};
    use changepacks_node::package::NodePackage;
    use tempfile::TempDir;
    use tokio::fs;

    use super::*;

    // Helper function to create a test project with dependencies
    fn create_project(name: &str, dependencies: Vec<&str>) -> Project {
        let mut package = NodePackage::new(
            Some(name.to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from(format!("/test/{}/package.json", name)),
            PathBuf::from(format!("{}/package.json", name)),
        );
        for dep in dependencies {
            package.add_dependency(dep);
        }
        Project::Package(Box::new(package))
    }

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
            assert!(gen_update_map(temp_path, &config).await.unwrap().is_empty());
        }
        {
            fs::write(
                changepacks_dir.join("config.json"),
                serde_json::to_string(&Config::default()).unwrap(),
            )
            .await
            .unwrap();
            assert!(gen_update_map(temp_path, &config).await.unwrap().is_empty());
        }
        {
            fs::write(changepacks_dir.join("wrong.file"), "{}")
                .await
                .unwrap();
            assert!(gen_update_map(temp_path, &config).await.unwrap().is_empty());
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
            let update_map = gen_update_map(temp_path, &config).await.unwrap();
            assert!(update_map.len() == 1);
            assert!(update_map.contains_key(&temp_path.join("package")));
            assert!(update_map[&temp_path.join("package")].0 == UpdateType::Patch);
        }

        {
            let update_map = gen_update_map(temp_path, &config).await.unwrap();
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
            let update_map = gen_update_map(temp_path, &config).await.unwrap();
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
            let update_map = gen_update_map(temp_path, &config).await.unwrap();
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
            let update_map = gen_update_map(temp_path, &config).await.unwrap();
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

        let update_map = gen_update_map(temp_path, &config).await.unwrap();

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

    #[test]
    fn test_apply_reverse_dependencies_basic() {
        // Setup: core has no deps, cli depends on core
        let core = create_project("core", vec![]);
        let cli = create_project("cli", vec!["core"]);

        let projects: Vec<&Project> = vec![&core, &cli];
        let repo_root = Path::new("/test");

        // Core is being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

        // cli should be added as PATCH update
        assert_eq!(update_map.len(), 2);
        assert!(update_map.contains_key(&PathBuf::from("cli/package.json")));
        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Patch
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_transitive() {
        // Setup: core -> utils -> cli (cli depends on utils, utils depends on core)
        let core = create_project("core", vec![]);
        let utils = create_project("utils", vec!["core"]);
        let cli = create_project("cli", vec!["utils"]);

        let projects: Vec<&Project> = vec![&core, &utils, &cli];
        let repo_root = Path::new("/test");

        // Core is being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

        // Both utils and cli should be added as PATCH updates (transitive)
        assert_eq!(update_map.len(), 3);
        assert!(update_map.contains_key(&PathBuf::from("utils/package.json")));
        assert!(update_map.contains_key(&PathBuf::from("cli/package.json")));
        assert_eq!(
            update_map[&PathBuf::from("utils/package.json")].0,
            UpdateType::Patch
        );
        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Patch
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_no_deps() {
        // Setup: two independent packages
        let core = create_project("core", vec![]);
        let utils = create_project("utils", vec![]);

        let projects: Vec<&Project> = vec![&core, &utils];
        let repo_root = Path::new("/test");

        // Core is being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

        // utils should NOT be added (no dependency on core)
        assert_eq!(update_map.len(), 1);
        assert!(!update_map.contains_key(&PathBuf::from("utils/package.json")));
    }

    #[test]
    fn test_apply_reverse_dependencies_already_in_update_map() {
        // Setup: cli depends on core, but cli is already in update_map
        let core = create_project("core", vec![]);
        let cli = create_project("cli", vec!["core"]);

        let projects: Vec<&Project> = vec![&core, &cli];
        let repo_root = Path::new("/test");

        // Both core and cli are already being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );
        update_map.insert(
            PathBuf::from("cli/package.json"),
            (
                UpdateType::Major,
                vec![ChangePackResultLog::new(
                    UpdateType::Major,
                    "Update cli".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

        // cli should remain Major (not overwritten to Patch)
        assert_eq!(update_map.len(), 2);
        assert_eq!(
            update_map[&PathBuf::from("cli/package.json")].0,
            UpdateType::Major
        );
    }

    #[test]
    fn test_apply_reverse_dependencies_diamond() {
        // Diamond pattern: core <- (utils, helpers) <- cli
        // cli depends on both utils and helpers, both depend on core
        let core = create_project("core", vec![]);
        let utils = create_project("utils", vec!["core"]);
        let helpers = create_project("helpers", vec!["core"]);
        let cli = create_project("cli", vec!["utils", "helpers"]);

        let projects: Vec<&Project> = vec![&core, &utils, &helpers, &cli];
        let repo_root = Path::new("/test");

        // Core is being updated
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("core/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

        // All packages should be updated
        assert_eq!(update_map.len(), 4);
        assert!(update_map.contains_key(&PathBuf::from("utils/package.json")));
        assert!(update_map.contains_key(&PathBuf::from("helpers/package.json")));
        assert!(update_map.contains_key(&PathBuf::from("cli/package.json")));
    }

    #[test]
    fn test_apply_reverse_dependencies_missing_dependency() {
        // cli depends on "missing" package that doesn't exist in projects
        let cli = create_project("cli", vec!["missing"]);

        let projects: Vec<&Project> = vec![&cli];
        let repo_root = Path::new("/test");

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("other/package.json"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update other".to_string(),
                )],
            ),
        );

        apply_reverse_dependencies(&mut update_map, &projects, repo_root);

        // No changes, missing dependency is ignored
        assert_eq!(update_map.len(), 1);
    }

    #[test]
    fn test_apply_update_on_rules_invalid_pattern() {
        // Test with invalid glob pattern
        let mut update_on = HashMap::new();
        update_on.insert(
            "[invalid".to_string(), // Invalid glob pattern
            vec!["bridge/node".to_string()],
        );
        let config = Config {
            update_on,
            ..Default::default()
        };

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_update_on_rules(&mut update_map, &config);

        // Should still have only the original entry (invalid pattern is skipped)
        assert_eq!(update_map.len(), 1);
    }

    #[test]
    fn test_apply_update_on_rules_no_match() {
        // Test when no package matches the trigger pattern
        let mut update_on = HashMap::new();
        update_on.insert("other/*".to_string(), vec!["bridge/node".to_string()]);
        let config = Config {
            update_on,
            ..Default::default()
        };

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );

        apply_update_on_rules(&mut update_map, &config);

        // Should still have only the original entry (no match)
        assert_eq!(update_map.len(), 1);
    }

    #[test]
    fn test_apply_update_on_rules_dependent_already_exists() {
        // Test when dependent package is already in update_map
        let mut update_on = HashMap::new();
        update_on.insert("crates/*".to_string(), vec!["bridge/node".to_string()]);
        let config = Config {
            update_on,
            ..Default::default()
        };

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );
        update_map.insert(
            PathBuf::from("bridge/node"),
            (
                UpdateType::Major,
                vec![ChangePackResultLog::new(
                    UpdateType::Major,
                    "Update bridge".to_string(),
                )],
            ),
        );

        apply_update_on_rules(&mut update_map, &config);

        // bridge/node should remain Major (not overwritten to Patch)
        assert_eq!(update_map.len(), 2);
        assert_eq!(
            update_map[&PathBuf::from("bridge/node")].0,
            UpdateType::Major
        );
    }
}
