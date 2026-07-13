use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug)]
pub struct GradlePackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl GradlePackage {
    // Standard package/workspace constructor.
    changepacks_core::impl_default_new!();
}

#[async_trait]
impl Package for GradlePackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // Route version calculation through the shared bump helper (matching
    // Node/Dart) and leave only the Gradle file rewrite to the Java writer.
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let path = &self.path;
        changepacks_utils::bump_version_with(&mut self.version, path, update_type, async |new| {
            crate::write_gradle_version(path, new).await
        })
        .await
    }

    // Fixed language accessor.
    changepacks_core::impl_language!(Language::Java);

    // Per-OS command lives on the const in `crate` (see `lib.rs`). Gradle's
    // `--dry-run` flag only previews the task graph without executing
    // tasks, so it cannot validate the publishing pipeline;
    // `publishToMavenLocal` is the closest functional equivalent: it runs
    // the entire publish flow (configuration, artifact generation, POM
    // generation) but writes to `~/.m2/repository` instead of uploading
    // to a remote registry.
    changepacks_core::impl_const_publish_commands!(
        crate::PUBLISH_COMMAND,
        crate::DRY_RUN_PUBLISH_COMMAND
    );

    // Dependency set accessors.
    changepacks_core::impl_dependencies_accessors!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::UpdateType;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    fn assert_gradle_package_defaults(package: &GradlePackage) {
        assert_eq!(package.name(), Some("test-package"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), PathBuf::from("/test/build.gradle.kts"));
        assert_eq!(
            package.relative_path(),
            PathBuf::from("test/build.gradle.kts")
        );
        assert_eq!(package.language(), Language::Java);
        assert!(!package.is_changed());
        #[cfg(windows)]
        {
            assert_eq!(package.default_publish_command(), ".\\gradlew.bat publish");
            assert_eq!(
                package.default_dry_run_publish_command().as_deref(),
                Some(".\\gradlew.bat publishToMavenLocal")
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(package.default_publish_command(), "./gradlew publish");
            assert_eq!(
                package.default_dry_run_publish_command().as_deref(),
                Some("./gradlew publishToMavenLocal")
            );
        }
    }

    #[tokio::test]
    async fn test_gradle_package_new() {
        let package = GradlePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        );

        assert_gradle_package_defaults(&package);
    }

    #[tokio::test]
    async fn test_gradle_package_set_changed() {
        let mut package = GradlePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        );

        assert!(!package.is_changed());
        package.set_changed(true);
        assert!(package.is_changed());
        package.set_changed(false);
        assert!(!package.is_changed());
    }

    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_gradle_package_update_version_kts(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
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

        let mut package = GradlePackage::new(
            Some("myproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("myproject/build.gradle.kts"),
        );

        package.update_version(update_type).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains(&format!(r#"version = "{expected}""#)));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_package_update_version_groovy() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
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

        let mut package = GradlePackage::new(
            Some("myproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("myproject/build.gradle"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains("version = '1.0.1'"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_package_update_version_with_fallback() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(
            &build_gradle,
            r#"
group = "com.devfive"
version = project.findProperty("releaseVersion") ?: "1.0.11"
"#,
        )
        .unwrap();

        let mut package = GradlePackage::new(
            Some("myproject".to_string()),
            Some("1.0.11".to_string()),
            build_gradle.clone(),
            PathBuf::from("myproject/build.gradle.kts"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains(r#"?: "1.0.12""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_package_update_version_errors_without_editable_version() {
        let temp_dir = TempDir::new().unwrap();
        let build_gradle = temp_dir.path().join("build.gradle.kts");
        let content = "plugins { id(\"java\") }\ngroup = \"com.example\"\n";
        fs::write(&build_gradle, content).unwrap();
        let bytes_before = fs::read(&build_gradle).unwrap();
        let mut package = GradlePackage::new(
            Some("myproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("build.gradle.kts"),
        );

        let result = package.update_version(UpdateType::Patch).await;

        assert!(result.is_err());
        assert_eq!(fs::read(&build_gradle).unwrap(), bytes_before);
        assert_eq!(package.version(), Some("1.0.0"));
    }

    #[test]
    fn test_gradle_package_dependencies() {
        let mut package = GradlePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        );

        // Initially empty
        assert!(package.dependencies().is_empty());

        // Add dependencies
        package.add_dependency("core");
        package.add_dependency("utils");

        let deps = package.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("core"));
        assert!(deps.contains("utils"));

        // Adding duplicate should not increase count
        package.add_dependency("core");
        assert_eq!(package.dependencies().len(), 2);
    }

    #[test]
    fn test_set_name() {
        let mut package = GradlePackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("build.gradle.kts"),
        );
        assert_eq!(package.name(), None);
        package.set_name("my-project".to_string());
        assert_eq!(package.name(), Some("my-project"));
    }
}
