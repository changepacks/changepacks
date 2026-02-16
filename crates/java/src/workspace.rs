use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use changepacks_utils::next_version;
use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};

#[derive(Debug)]
pub struct GradleWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl GradleWorkspace {
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self {
            path,
            relative_path,
            name,
            version,
            is_changed: false,
            dependencies: HashSet::new(),
        }
    }
}

/// Update version in build.gradle.kts content
fn update_version_kts(content: &str, new_version: &str) -> String {
    // Pattern 1: version = "1.0.0"
    let simple_pattern = Regex::new(r#"(?m)^(version\s*=\s*)"[^"]+""#).unwrap();
    if simple_pattern.is_match(content) {
        return simple_pattern
            .replace(content, format!(r#"${{1}}"{new_version}""#))
            .to_string();
    }

    // Pattern 2: version = project.findProperty("...") ?: "1.0.0"
    let fallback_pattern =
        Regex::new(r#"(?m)^(version\s*=\s*project\.findProperty\([^)]+\)\s*\?:\s*)"[^"]+""#)
            .unwrap();
    if fallback_pattern.is_match(content) {
        return fallback_pattern
            .replace(content, format!(r#"${{1}}"{new_version}""#))
            .to_string();
    }

    content.to_string()
}

/// Update version in build.gradle (Groovy) content
fn update_version_groovy(content: &str, new_version: &str) -> String {
    // Pattern 1: version = '1.0.0' or version = "1.0.0"
    let assign_pattern = Regex::new(r#"(?m)^(version\s*=\s*)['"][^'"]+['"]"#).unwrap();
    if assign_pattern.is_match(content) {
        return assign_pattern
            .replace(content, format!(r#"${{1}}'{new_version}'"#))
            .to_string();
    }

    // Pattern 2: version '1.0.0' or version "1.0.0"
    let space_pattern = Regex::new(r#"(?m)^(version\s+)['"][^'"]+['"]"#).unwrap();
    if space_pattern.is_match(content) {
        return space_pattern
            .replace(content, format!(r#"${{1}}'{new_version}'"#))
            .to_string();
    }

    content.to_string()
}

#[async_trait]
impl Workspace for GradleWorkspace {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let current_version = self.version.as_deref().unwrap_or("0.0.0");
        let new_version = next_version(current_version, update_type)?;

        let content = read_to_string(&self.path).await?;
        let file_name = self
            .path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or_default();
        let is_kts = file_name.ends_with(".kts");

        let updated_content = if is_kts {
            update_version_kts(&content, &new_version)
        } else {
            update_version_groovy(&content, &new_version)
        };

        write(&self.path, updated_content).await?;
        self.version = Some(new_version);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Java
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    fn default_publish_command(&self) -> String {
        "./gradlew publish".to_string()
    }

    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }

    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::UpdateType;
    use std::fs;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    #[tokio::test]
    async fn test_gradle_workspace_new() {
        let workspace = GradleWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        );

        assert_eq!(workspace.name(), Some("test-workspace"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), PathBuf::from("/test/build.gradle.kts"));
        assert_eq!(
            workspace.relative_path(),
            PathBuf::from("test/build.gradle.kts")
        );
        assert_eq!(workspace.language(), Language::Java);
        assert!(!workspace.is_changed());
        assert_eq!(workspace.default_publish_command(), "./gradlew publish");
    }

    #[tokio::test]
    async fn test_gradle_workspace_new_without_name_and_version() {
        let workspace = GradleWorkspace::new(
            None,
            None,
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
    }

    #[tokio::test]
    async fn test_gradle_workspace_set_changed() {
        let mut workspace = GradleWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        );

        assert!(!workspace.is_changed());
        workspace.set_changed(true);
        assert!(workspace.is_changed());
        workspace.set_changed(false);
        assert!(!workspace.is_changed());
    }

    #[tokio::test]
    async fn test_gradle_workspace_update_version_kts_patch() {
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

        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("multiproject/build.gradle.kts"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains(r#"version = "1.0.1""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_workspace_update_version_kts_minor() {
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

        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("multiproject/build.gradle.kts"),
        );

        workspace.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains(r#"version = "1.1.0""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_workspace_update_version_kts_major() {
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

        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("multiproject/build.gradle.kts"),
        );

        workspace.update_version(UpdateType::Major).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains(r#"version = "2.0.0""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_workspace_update_version_groovy() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("multiproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle");
        fs::write(
            &build_gradle,
            r#"
plugins {
    id 'java'
}

group = 'com.example'
version = '1.0.0'
"#,
        )
        .unwrap();

        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("multiproject/build.gradle"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains("version = '1.0.1'"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_workspace_update_version_without_version() {
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
version = "0.0.0"
"#,
        )
        .unwrap();

        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            None,
            build_gradle.clone(),
            PathBuf::from("multiproject/build.gradle.kts"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains(r#"version = "0.0.1""#));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gradle_workspace_dependencies() {
        let mut workspace = GradleWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        );

        // Initially empty
        assert!(workspace.dependencies().is_empty());

        // Add dependencies
        workspace.add_dependency("core");
        workspace.add_dependency("utils");

        let deps = workspace.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("core"));
        assert!(deps.contains("utils"));

        // Adding duplicate should not increase count
        workspace.add_dependency("core");
        assert_eq!(workspace.dependencies().len(), 2);
    }
}
