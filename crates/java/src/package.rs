use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::Config;
use changepacks_core::{Language, Package, UpdateType};

// Nine-field declaration plus the three-step constructor chain, shared
// verbatim with `GradleWorkspace` (see `declare_gradle_project!` in `lib.rs`).
crate::declare_gradle_project!(pub struct GradlePackage);

#[async_trait]
impl Package for GradlePackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // Body shared verbatim with `GradleWorkspace::update_version` via
    // `crate::bump_gradle_version`; only the scope differs. A package owns
    // exactly one project, so only an outermost declaration is editable.
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::bump_gradle_version(
            &mut self.version,
            &self.path,
            update_type,
            crate::version_updater::GradleVersionScope::ScriptOnly,
        )
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

    // Publish-task flag accessors shared verbatim with `GradleWorkspace`.
    // Both are plain sync methods, so a crate-local macro works here (see
    // `impl_gradle_publish_task_flags!` in `lib.rs`).
    crate::impl_gradle_publish_task_flags!();

    // The two methods below are byte-identical to `GradleWorkspace`'s apart
    // from the directory-not-found message, but they are deliberately NOT
    // folded into a crate-local macro: `publish` / `dry_run_publish` are
    // `#[async_trait]` methods, and `async_trait` rewrites the `impl`
    // block before `macro_rules!` bodies expand, so a macro invocation
    // here would emit a plain `async fn` that no longer matches the
    // desugared trait signature (E0195). Emitting the desugared
    // `Pin<Box<dyn Future>>` form from a macro instead compiles, but costs
    // more lines than it saves and hides two trivial delegations behind
    // hand-written `async_trait` boilerplate.
    async fn publish(&self, config: &Config) -> Result<changepacks_core::publish::PublishOutput> {
        crate::run_publish_for_path(
            self.path(),
            self.relative_path(),
            self.project_path.as_deref(),
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
            self.project_path.as_deref(),
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
    use crate::test_support::{assert_reported_cwd, captured_argv, create_publish_wrapper};
    use changepacks_core::{Config, UpdateType};
    use rstest::rstest;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    fn shell_echo_command(message: &str) -> String {
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
        assert!(package.is_publishable_by_default());
        assert!(package.is_dry_run_publishable_by_default());
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

    #[test]
    fn test_gradle_package_publishability_tracks_available_tasks() {
        let remote_only = GradlePackage::new_with_publish_tasks(
            Some("remote-only".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
            true,
            false,
        );
        let local_only = GradlePackage::new_with_publish_tasks(
            Some("local-only".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
            false,
            true,
        );

        assert!(remote_only.is_publishable_by_default());
        assert!(!remote_only.is_dry_run_publishable_by_default());
        assert!(!local_only.is_publishable_by_default());
        assert!(local_only.is_dry_run_publishable_by_default());
    }

    #[test]
    fn test_gradle_package_set_changed() {
        changepacks_core::assert_set_changed_roundtrip!(GradlePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        ));
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
        assert_reported_cwd(&output.stdout, &root);
        assert_eq!(captured_argv(&output.stdout), [":libs:core:publish"]);
        assert!(dry_run.success, "stderr: {}", dry_run.stderr);
        let dry_run_argv = captured_argv(&dry_run.stdout);
        assert_eq!(dry_run_argv.len(), 2, "stdout: {}", dry_run.stdout);
        assert_eq!(dry_run_argv[0], ":libs:core:publishToMavenLocal");
        assert!(dry_run_argv[1].starts_with("-Dmaven.repo.local="));
    }

    #[tokio::test]
    async fn test_public_constructor_uses_root_tasks_for_nested_project_owning_wrapper() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("tools").join("standalone");
        fs::create_dir_all(&project_dir).unwrap();
        create_publish_wrapper(&project_dir);
        let manifest = project_dir.join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        let package = GradlePackage::new(
            Some("standalone".to_string()),
            Some("1.0.0".to_string()),
            manifest,
            PathBuf::from("tools/standalone/build.gradle.kts"),
        );

        let output = package.publish(&Config::default()).await.unwrap();
        let dry_run = package
            .dry_run_publish(&Config::default())
            .await
            .unwrap()
            .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert_eq!(captured_argv(&output.stdout), ["publish"]);
        assert!(dry_run.success, "stderr: {}", dry_run.stderr);
        let dry_run_argv = captured_argv(&dry_run.stdout);
        assert_eq!(dry_run_argv.len(), 2, "stdout: {}", dry_run.stdout);
        assert_eq!(dry_run_argv[0], "publishToMavenLocal");
        assert!(dry_run_argv[1].starts_with("-Dmaven.repo.local="));
    }

    #[tokio::test]
    async fn test_publish_filesystem_remapped_project_uses_exact_gradle_project_path_argv() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path().join("repo with spaces");
        let project_dir = root.join("generated-backend");
        fs::create_dir_all(&project_dir).unwrap();
        create_publish_wrapper(&root);
        let manifest = project_dir.join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        let package = GradlePackage::new_with_project_path_and_publish_tasks(
            Some("api".to_string()),
            Some("1.0.0".to_string()),
            manifest,
            PathBuf::from("generated-backend/build.gradle.kts"),
            Some(":api".to_string()),
            true,
            true,
        );

        let output = package.publish(&Config::default()).await.unwrap();
        let dry_run = package
            .dry_run_publish(&Config::default())
            .await
            .unwrap()
            .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert_eq!(captured_argv(&output.stdout), [":api:publish"]);
        assert!(dry_run.success, "stderr: {}", dry_run.stderr);
        let dry_run_argv = captured_argv(&dry_run.stdout);
        assert_eq!(dry_run_argv.len(), 2, "stdout: {}", dry_run.stdout);
        assert_eq!(dry_run_argv[0], ":api:publishToMavenLocal");
        assert!(dry_run_argv[1].starts_with("-Dmaven.repo.local="));
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
        assert_eq!(package.version(), Some(expected));

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
            r"
plugins {
    id 'java'
}

group = 'com.example'
version = '1.0.0'
",
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
        assert_eq!(package.version(), Some("1.0.1"));

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
    async fn gradle_package_updates_kotlin_version_from_sibling_properties() {
        let temp_dir = TempDir::new().unwrap();
        let build_gradle = temp_dir.path().join("build.gradle.kts");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"plugins { id(\"java\") }\r\n";
        let properties_content = b"version = 1.0.0 # package\r\n";
        tokio::fs::write(&build_gradle, build_content)
            .await
            .unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();
        let mut package = GradlePackage::new(
            Some("myproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("build.gradle.kts"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        assert_eq!(package.version(), Some("1.0.1"));
        assert_eq!(tokio::fs::read(&build_gradle).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            b"version = 1.0.1 # package\r\n"
        );
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

    #[tokio::test]
    async fn test_gradle_package_ambiguous_version_keeps_file_and_state_unchanged() {
        let temp_dir = TempDir::new().unwrap();
        let build_gradle = temp_dir.path().join("build.gradle.kts");
        let content = "version = \"1.0.0\"\nversion = \"duplicate\"\n";
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

    #[tokio::test]
    async fn test_gradle_package_rejects_allprojects_only_version() {
        let temp_dir = TempDir::new().unwrap();
        let build_gradle = temp_dir.path().join("build.gradle.kts");
        let content = "allprojects {\n    version = \"1.0.0\"\n}\n";
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
        changepacks_core::assert_dependencies_roundtrip!(
            GradlePackage::new(
                Some("test-package".to_string()),
                Some("1.0.0".to_string()),
                PathBuf::from("/test/build.gradle.kts"),
                PathBuf::from("test/build.gradle.kts"),
            ),
            "core",
            "utils"
        );
    }

    #[test]
    fn test_set_name() {
        changepacks_core::assert_set_name_roundtrip!(GradlePackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("build.gradle.kts"),
        ));
    }
}
