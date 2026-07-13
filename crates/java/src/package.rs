use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::Config;
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

    async fn publish(&self, config: &Config) -> Result<changepacks_core::publish::PublishOutput> {
        crate::run_publish_for_path(
            self.path(),
            self.relative_path(),
            config,
            changepacks_core::publish::PACKAGE_DIR_NOT_FOUND,
        )
        .await
    }

    async fn dry_run_publish(
        &self,
        config: &Config,
    ) -> Result<Option<changepacks_core::publish::PublishOutput>> {
        crate::run_dry_run_publish_for_path(
            self.path(),
            self.relative_path(),
            config,
            changepacks_core::publish::PACKAGE_DIR_NOT_FOUND,
        )
        .await
    }

    // Dependency set accessors.
    changepacks_core::impl_dependencies_accessors!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::{Config, UpdateType};
    use rstest::rstest;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    fn create_publish_wrapper(root: &Path) {
        #[cfg(windows)]
        fs::write(
            root.join("gradlew.bat"),
            "@echo off\necho cwd=%CD%\necho args=%*\n",
        )
        .unwrap();

        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;

            let wrapper = root.join("gradlew");
            fs::write(
                &wrapper,
                "#!/bin/sh\nprintf 'cwd=%s\\nargs=%s\\n' \"$PWD\" \"$*\"\n",
            )
            .unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn shell_echo_command(message: &str) -> String {
        #[cfg(windows)]
        return format!("echo {message} && echo shell-override");

        #[cfg(not(windows))]
        format!("echo {message} && echo shell-override")
    }

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

    #[tokio::test]
    async fn test_publish_nested_project_uses_wrapper_root_and_exact_task_path() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path().join("repo with spaces");
        let project_dir = root.join("libs").join("core");
        fs::create_dir_all(&project_dir).unwrap();
        create_publish_wrapper(&root);
        let manifest = project_dir.join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        let package = GradlePackage::new(
            Some("core".to_string()),
            Some("1.0.0".to_string()),
            manifest,
            PathBuf::from("libs/core/build.gradle.kts"),
        );

        let output = package.publish(&Config::default()).await.unwrap();
        let dry_run = package
            .dry_run_publish(&Config::default())
            .await
            .unwrap()
            .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains(&format!("cwd={}", root.display())));
        assert!(output.stdout.contains("args=:libs:core:publish"));
        assert!(dry_run.success, "stderr: {}", dry_run.stderr);
        assert!(
            dry_run
                .stdout
                .contains("args=:libs:core:publishToMavenLocal")
        );
    }

    #[tokio::test]
    async fn test_publish_default_errors_when_platform_wrapper_is_missing() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        let package = GradlePackage::new(
            Some("root".to_string()),
            Some("1.0.0".to_string()),
            manifest,
            PathBuf::from("build.gradle.kts"),
        );

        let error = package.publish(&Config::default()).await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Gradle wrapper (gradlew) not found")
        );
    }

    #[tokio::test]
    async fn test_publish_path_override_keeps_shell_execution_and_skips_wrapper_lookup() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        let package = GradlePackage::new(
            Some("root".to_string()),
            Some("1.0.0".to_string()),
            manifest,
            PathBuf::from("build.gradle.kts"),
        );
        let mut publish = BTreeMap::new();
        publish.insert(
            "build.gradle.kts".to_string(),
            shell_echo_command("path-override"),
        );
        publish.insert("java".to_string(), shell_echo_command("language-override"));
        let config = Config {
            publish,
            ..Default::default()
        };

        let output = package.publish(&config).await.unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("path-override"));
        assert!(output.stdout.contains("shell-override"));
        assert!(!output.stdout.contains("language-override"));
    }

    #[tokio::test]
    async fn test_dry_run_publish_language_override_keeps_shell_execution() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        let package = GradlePackage::new(
            Some("root".to_string()),
            Some("1.0.0".to_string()),
            manifest,
            PathBuf::from("build.gradle.kts"),
        );
        let mut publish_dry_run = BTreeMap::new();
        publish_dry_run.insert("java".to_string(), shell_echo_command("language-dry-run"));
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        let output = package.dry_run_publish(&config).await.unwrap().unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("language-dry-run"));
        assert!(output.stdout.contains("shell-override"));
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
