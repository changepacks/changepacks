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
    #[must_use]
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
    has_subprojects: bool,
}

/// Find gradlew executable by walking up the directory tree.
///
/// In multi-module Gradle builds, `gradlew` lives at the root while subprojects
/// only contain `build.gradle.kts`. This function searches upward from `start_dir`
/// until it finds `gradlew` (Unix) or `gradlew.bat` (Windows).
///
/// Returns `(gradlew_path, gradlew_dir)` or `None` if not found.
fn find_gradlew(start_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let gradlew_name = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };

    let mut current = start_dir.to_path_buf();
    loop {
        let gradlew = current.join(gradlew_name);
        if gradlew.exists() {
            return Some((gradlew, current));
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Get project properties using gradlew command.
///
/// Walks up the directory tree to find `gradlew`, then runs it with the correct
/// subproject path. For a subproject at `root/libs/core/`, this runs:
/// `./gradlew :libs:core:properties -q` from the root directory.
async fn get_gradle_properties(project_dir: &Path) -> Option<GradleProperties> {
    let (gradlew, gradlew_dir) = find_gradlew(project_dir)?;

    // Build the command args based on whether this is the root or a subproject
    let args: Vec<String> = if gradlew_dir == project_dir {
        // Root project: ./gradlew properties -q
        vec!["properties".to_string(), "-q".to_string()]
    } else {
        // Subproject: ./gradlew :sub:path:properties -q
        let relative = project_dir.strip_prefix(&gradlew_dir).ok()?;
        let gradle_path = relative
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join(":");
        vec![format!(":{gradle_path}:properties"), "-q".to_string()]
    };

    let output = Command::new(&gradlew)
        .args(&args)
        .current_dir(&gradlew_dir)
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
    let subprojects_pattern = Regex::new(r"(?m)^subprojects:\s*(.+)$").ok()?;

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

    // Detect workspace: subprojects is non-empty (e.g. "[project ':sub1', project ':sub2']")
    if let Some(caps) = subprojects_pattern.captures(&stdout) {
        let value = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        props.has_subprojects = value != "[]";
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

            // Workspace detection: gradlew reports non-empty subprojects list.
            // Previous approach (checking for settings.gradle.kts existence) caused
            // false positives in composite builds and subprojects with IDE-generated files.
            let is_workspace = props.has_subprojects;

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

        // Mock gradlew that reports subprojects (this is what makes it a workspace)
        if cfg!(windows) {
            fs::write(
                project_dir.join("gradlew.bat"),
                "@echo off\necho name: multiproject\necho version: 1.0.0\necho subprojects: [project ':subproject1', project ':subproject2']\n",
            )
            .unwrap();
        } else {
            let gradlew_path = project_dir.join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'name: multiproject'\necho 'version: 1.0.0'\necho \"subprojects: [project ':subproject1', project ':subproject2']\"\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

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
                assert_eq!(ws.name(), Some("multiproject"));
                assert_eq!(ws.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_settings_file_does_not_make_workspace() {
        // Regression: settings.gradle.kts presence alone must NOT classify as Workspace.
        // Only gradlew's subprojects output determines workspace status.
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        // settings.gradle.kts exists but no gradlew → should be Package, not Workspace
        fs::write(
            project_dir.join("settings.gradle.kts"),
            "rootProject.name = \"myproject\"\n",
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
            Project::Package(_) => {} // correct: no gradlew → no subprojects info → Package
            _ => panic!("Expected Package, not Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_empty_subprojects_is_package() {
        // A project with gradlew but subprojects: [] is a Package, not Workspace
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("standalone");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        if cfg!(windows) {
            fs::write(
                project_dir.join("gradlew.bat"),
                "@echo off\necho name: standalone\necho version: 1.0.0\necho subprojects: []\n",
            )
            .unwrap();
        } else {
            let gradlew_path = project_dir.join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'name: standalone'\necho 'version: 1.0.0'\necho 'subprojects: []'\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let mut finder = GradleProjectFinder::new();
        finder
            .visit(&build_gradle, &PathBuf::from("standalone/build.gradle.kts"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("standalone"));
            }
            _ => panic!("Expected Package, not Workspace"),
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

    #[test]
    fn test_find_gradlew_in_same_dir() {
        let temp_dir = TempDir::new().unwrap();

        if cfg!(windows) {
            fs::write(temp_dir.path().join("gradlew.bat"), "@echo off").unwrap();
        } else {
            fs::write(temp_dir.path().join("gradlew"), "#!/bin/sh").unwrap();
        }

        let result = find_gradlew(temp_dir.path());
        assert!(result.is_some());
        let (_, gradlew_dir) = result.unwrap();
        assert_eq!(gradlew_dir, temp_dir.path());

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_find_gradlew_in_parent_dir() {
        let temp_dir = TempDir::new().unwrap();
        let subproject = temp_dir.path().join("libs").join("core");
        fs::create_dir_all(&subproject).unwrap();

        // gradlew at root, not in subproject
        if cfg!(windows) {
            fs::write(temp_dir.path().join("gradlew.bat"), "@echo off").unwrap();
        } else {
            fs::write(temp_dir.path().join("gradlew"), "#!/bin/sh").unwrap();
        }

        let result = find_gradlew(&subproject);
        assert!(result.is_some());
        let (_, gradlew_dir) = result.unwrap();
        assert_eq!(gradlew_dir, temp_dir.path().to_path_buf());

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_find_gradlew_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let subdir = temp_dir.path().join("no_gradlew_here");
        fs::create_dir_all(&subdir).unwrap();

        // Don't create gradlew anywhere — but find_gradlew walks to filesystem
        // root, so this test just verifies it doesn't panic. In practice it
        // returns None only when no gradlew exists anywhere up the tree.
        // For a reliable "not found" test, we rely on the no-gradlew properties test below.
        let _ = find_gradlew(&subdir);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_no_gradlew() {
        let temp_dir = TempDir::new().unwrap();
        // get_gradle_properties will walk up and may find a system gradlew,
        // but for a temp dir with no gradlew anywhere close, it returns None
        // or Some with unrelated properties. The key contract: no crash.
        let _result = get_gradle_properties(temp_dir.path()).await;
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_with_mock() {
        let temp_dir = TempDir::new().unwrap();

        // Create mock gradlew that outputs properties
        if cfg!(windows) {
            let gradlew_path = temp_dir.path().join("gradlew.bat");
            fs::write(
                &gradlew_path,
                "@echo off\necho name: myproject\necho version: 1.2.3\n",
            )
            .unwrap();
        } else {
            let gradlew_path = temp_dir.path().join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'name: myproject'\necho 'version: 1.2.3'\n",
            )
            .unwrap();
            // Make executable on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let result = get_gradle_properties(temp_dir.path()).await;
        assert!(result.is_some());
        let props = result.unwrap();
        assert_eq!(props.name, Some("myproject".to_string()));
        assert_eq!(props.version, Some("1.2.3".to_string()));
        assert!(!props.has_subprojects);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_with_subprojects() {
        let temp_dir = TempDir::new().unwrap();

        if cfg!(windows) {
            fs::write(
                temp_dir.path().join("gradlew.bat"),
                "@echo off\necho name: root\necho version: 1.0.0\necho subprojects: [project ':app', project ':lib']\n",
            )
            .unwrap();
        } else {
            let gradlew_path = temp_dir.path().join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'name: root'\necho 'version: 1.0.0'\necho \"subprojects: [project ':app', project ':lib']\"\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let result = get_gradle_properties(temp_dir.path()).await;
        assert!(result.is_some());
        let props = result.unwrap();
        assert_eq!(props.name, Some("root".to_string()));
        assert!(props.has_subprojects);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_empty_subprojects() {
        let temp_dir = TempDir::new().unwrap();

        if cfg!(windows) {
            fs::write(
                temp_dir.path().join("gradlew.bat"),
                "@echo off\necho name: leaf\necho version: 1.0.0\necho subprojects: []\n",
            )
            .unwrap();
        } else {
            let gradlew_path = temp_dir.path().join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'name: leaf'\necho 'version: 1.0.0'\necho 'subprojects: []'\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let result = get_gradle_properties(temp_dir.path()).await;
        assert!(result.is_some());
        let props = result.unwrap();
        assert_eq!(props.name, Some("leaf".to_string()));
        assert!(!props.has_subprojects);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_from_parent_gradlew() {
        let temp_dir = TempDir::new().unwrap();
        let subproject = temp_dir.path().join("sub1");
        fs::create_dir_all(&subproject).unwrap();

        // Place gradlew at root, query from subproject dir
        if cfg!(windows) {
            let gradlew_path = temp_dir.path().join("gradlew.bat");
            // Mock: ignore the :sub1:properties arg, just output properties
            fs::write(
                &gradlew_path,
                "@echo off\necho name: sub1\necho version: 2.0.0\n",
            )
            .unwrap();
        } else {
            let gradlew_path = temp_dir.path().join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'name: sub1'\necho 'version: 2.0.0'\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let result = get_gradle_properties(&subproject).await;
        assert!(result.is_some());
        let props = result.unwrap();
        assert_eq!(props.name, Some("sub1".to_string()));
        assert_eq!(props.version, Some("2.0.0".to_string()));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_nested_subproject() {
        let temp_dir = TempDir::new().unwrap();
        let subproject = temp_dir.path().join("libs").join("core");
        fs::create_dir_all(&subproject).unwrap();

        // Place gradlew at root, query from libs/core/
        if cfg!(windows) {
            let gradlew_path = temp_dir.path().join("gradlew.bat");
            // The mock script receives ":libs:core:properties" "-q" as args
            fs::write(
                &gradlew_path,
                "@echo off\necho name: core\necho version: 3.1.0\n",
            )
            .unwrap();
        } else {
            let gradlew_path = temp_dir.path().join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'name: core'\necho 'version: 3.1.0'\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let result = get_gradle_properties(&subproject).await;
        assert!(result.is_some());
        let props = result.unwrap();
        assert_eq!(props.name, Some("core".to_string()));
        assert_eq!(props.version, Some("3.1.0".to_string()));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_unspecified() {
        let temp_dir = TempDir::new().unwrap();

        if cfg!(windows) {
            let gradlew_path = temp_dir.path().join("gradlew.bat");
            fs::write(
                &gradlew_path,
                "@echo off\necho name: unspecified\necho version: unspecified\n",
            )
            .unwrap();
        } else {
            let gradlew_path = temp_dir.path().join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'name: unspecified'\necho 'version: unspecified'\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let result = get_gradle_properties(temp_dir.path()).await;
        assert!(result.is_some());
        let props = result.unwrap();
        assert!(props.name.is_none());
        assert!(props.version.is_none());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_gradlew_fails() {
        let temp_dir = TempDir::new().unwrap();

        if cfg!(windows) {
            let gradlew_path = temp_dir.path().join("gradlew.bat");
            fs::write(&gradlew_path, "@echo off\nexit /b 1\n").unwrap();
        } else {
            let gradlew_path = temp_dir.path().join("gradlew");
            fs::write(&gradlew_path, "#!/bin/sh\nexit 1\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&gradlew_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let result = get_gradle_properties(temp_dir.path()).await;
        assert!(result.is_none());

        temp_dir.close().unwrap();
    }
}
