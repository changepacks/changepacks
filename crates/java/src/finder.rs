use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use regex::Regex;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::process::Command;

use crate::{package::GradlePackage, workspace::GradleWorkspace};

#[derive(Debug)]
pub struct GradleProjectFinder {
    projects: HashMap<PathBuf, Project>,
    project_files: Vec<&'static str>,
}

impl Default for GradleProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl GradleProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec!["build.gradle.kts", "build.gradle"],
        }
    }
}

/// Project info obtained from gradlew properties
#[derive(Debug, Default)]
struct GradleProperties {
    name: Option<String>,
    version: Option<String>,
}

/// Get project properties using gradlew command
async fn get_gradle_properties(project_dir: &Path) -> Option<GradleProperties> {
    // Determine gradlew command based on OS
    let gradlew = if cfg!(windows) {
        project_dir.join("gradlew.bat")
    } else {
        project_dir.join("gradlew")
    };

    // Check if gradlew exists
    if !gradlew.exists() {
        return None;
    }

    // Run gradlew properties -q
    let output = Command::new(&gradlew)
        .args(["properties", "-q"])
        .current_dir(project_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut props = GradleProperties::default();

    // Parse properties output
    // Format: "propertyName: value"
    let name_pattern = Regex::new(r"(?m)^name:\s*(.+)$").ok()?;
    let version_pattern = Regex::new(r"(?m)^version:\s*(.+)$").ok()?;

    if let Some(caps) = name_pattern.captures(&stdout) {
        let name = caps.get(1).map(|m| m.as_str().trim().to_string());
        if name.as_deref() != Some("unspecified") {
            props.name = name;
        }
    }

    if let Some(caps) = version_pattern.captures(&stdout) {
        let version = caps.get(1).map(|m| m.as_str().trim().to_string());
        if version.as_deref() != Some("unspecified") {
            props.version = version;
        }
    }

    Some(props)
}

#[async_trait]
impl ProjectFinder for GradleProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }

    fn projects_mut(&mut self) -> Vec<&mut Project> {
        self.projects.values_mut().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &self.project_files
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        if path.is_file()
            && self.project_files().contains(
                &path
                    .file_name()
                    .context(format!("File name not found - {}", path.display()))?
                    .to_str()
                    .context(format!("File name not found - {}", path.display()))?,
            )
        {
            if self.projects.contains_key(path) {
                return Ok(());
            }

            let project_dir = path
                .parent()
                .context(format!("Parent not found - {}", path.display()))?;

            // Get properties from gradlew command
            let props = get_gradle_properties(project_dir).await.unwrap_or_default();

            // Use directory name as fallback for project name
            let name = props.name.or_else(|| {
                project_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(std::string::ToString::to_string)
            });

            let version = props.version;

            // Check if this is a multi-project build (has settings.gradle or settings.gradle.kts)
            let is_workspace = project_dir.join("settings.gradle.kts").is_file()
                || project_dir.join("settings.gradle").is_file();

            let (path, project) = if is_workspace {
                (
                    path.to_path_buf(),
                    Project::Workspace(Box::new(GradleWorkspace::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                )
            } else {
                (
                    path.to_path_buf(),
                    Project::Package(Box::new(GradlePackage::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                )
            };

            self.projects.insert(path, project);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::Project;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_gradle_project_finder_new() {
        let finder = GradleProjectFinder::new();
        assert_eq!(
            finder.project_files(),
            &["build.gradle.kts", "build.gradle"]
        );
        assert_eq!(finder.projects().len(), 0);
    }

    #[test]
    fn test_gradle_project_finder_default() {
        let finder = GradleProjectFinder::default();
        assert_eq!(
            finder.project_files(),
            &["build.gradle.kts", "build.gradle"]
        );
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_kts_package() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(
            &build_gradle,
            r#"
plugins {
    id("java")
}

group = "com.example"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = GradleProjectFinder::new();
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                // Without gradlew, falls back to directory name
                assert_eq!(pkg.name(), Some("myproject"));
                // Version is None without gradlew
                assert_eq!(pkg.version(), None);
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_groovy_package() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("groovyproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle");
        fs::write(
            &build_gradle,
            r#"
plugins {
    id 'java'
}

group = 'com.example'
version = '2.0.0'
"#,
        )
        .unwrap();

        let mut finder = GradleProjectFinder::new();
        finder
            .visit(&build_gradle, &PathBuf::from("groovyproject/build.gradle"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                // Without gradlew, falls back to directory name
                assert_eq!(pkg.name(), Some("groovyproject"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("multiproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(
            &build_gradle,
            r#"
plugins {
    id("java")
}

group = "com.example"
version = "1.0.0"
"#,
        )
        .unwrap();

        // Create settings.gradle.kts to mark as workspace
        let settings_gradle = project_dir.join("settings.gradle.kts");
        fs::write(
            &settings_gradle,
            r#"
rootProject.name = "multiproject"
include("subproject1", "subproject2")
"#,
        )
        .unwrap();

        let mut finder = GradleProjectFinder::new();
        finder
            .visit(
                &build_gradle,
                &PathBuf::from("multiproject/build.gradle.kts"),
            )
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Workspace(ws) => {
                // Without gradlew, falls back to directory name
                assert_eq!(ws.name(), Some("multiproject"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_non_gradle_file() {
        let temp_dir = TempDir::new().unwrap();
        let other_file = temp_dir.path().join("other.txt");
        fs::write(&other_file, "some content").unwrap();

        let mut finder = GradleProjectFinder::new();
        finder
            .visit(&other_file, &PathBuf::from("other.txt"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(
            &build_gradle,
            r#"
group = "com.example"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = GradleProjectFinder::new();
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        // Visit again - should not add duplicate
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_projects_mut() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(
            &build_gradle,
            r#"
group = "com.example"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = GradleProjectFinder::new();
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        let mut_projects = finder.projects_mut();
        assert_eq!(mut_projects.len(), 1);

        temp_dir.close().unwrap();
    }
}
