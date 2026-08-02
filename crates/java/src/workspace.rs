use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::Config;
use changepacks_core::{Language, UpdateType, Workspace};

// Nine-field declaration plus the three-step constructor chain, shared
// verbatim with `GradlePackage` (see `declare_gradle_project!` in `lib.rs`).
crate::declare_gradle_project!(pub struct GradleWorkspace);

#[async_trait]
impl Workspace for GradleWorkspace {
    // Seven basic accessors (`name`, `version`, `path`, `relative_path`,
    // `is_changed`, `set_changed`, `set_name`) share their byte-identical
    // bodies with every other language crate's `Package` / `Workspace`
    // impl. Consolidated via `impl_basic_accessors!()` in `changepacks-core`
    // — expansion is byte-identical to the previous hand-rolled bodies.
    changepacks_core::impl_basic_accessors!();

    // Body shared verbatim with `GradlePackage::update_version` via
    // `crate::bump_gradle_version`; only the scope differs. A workspace root
    // may also declare the version inside a top-level `allprojects { ... }`
    // block, which fans the bump out to every subproject.
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::bump_gradle_version(
            &mut self.version,
            &self.path,
            update_type,
            crate::version_updater::GradleVersionScope::ScriptAndAllProjects,
        )
        .await
    }

    // Byte-identical `fn language(&self) -> Language { Language::Java }`
    // one-liner shared with every other language crate's `Package` /
    // `Workspace` impl. Consolidated via `impl_language!()` in
    // `changepacks-core` alongside the other accessor macros.
    changepacks_core::impl_language!(Language::Java);

    // Per-OS command lives on the const in `crate` (see `lib.rs`). See the
    // Java package impl for the `publishToMavenLocal` dry-run rationale:
    // Gradle's `--dry-run` only previews the task graph, so we run the
    // full publish pipeline against the local Maven cache
    // (`~/.m2/repository`) instead.
    //
    // Consolidated via `impl_const_publish_commands!()` in
    // `changepacks-core` — expansion is byte-identical to the previous
    // hand-rolled bodies.
    changepacks_core::impl_const_publish_commands!(
        crate::PUBLISH_COMMAND,
        crate::DRY_RUN_PUBLISH_COMMAND
    );

    // See the matching note on the Java package impl: these four methods
    // stay hand-written because `#[async_trait]` rewrites the `impl` block
    // before `macro_rules!` bodies expand, so a macro-emitted `async fn`
    // no longer matches the desugared trait signature (E0195).
    fn is_publishable_by_default(&self) -> bool {
        self.has_publish_task
    }

    fn is_dry_run_publishable_by_default(&self) -> bool {
        self.has_publish_to_maven_local_task
    }

    async fn publish(&self, config: &Config) -> Result<changepacks_core::publish::PublishOutput> {
        crate::run_publish_for_path(
            self.path(),
            self.relative_path(),
            self.project_path.as_deref(),
            config,
            changepacks_core::publish::WORKSPACE_DIR_NOT_FOUND,
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
            changepacks_core::publish::WORKSPACE_DIR_NOT_FOUND,
        )
        .await
    }

    // `dependencies()` / `add_dependency()` share their byte-identical
    // body with every other language crate's `Package` and `Workspace`
    // impl (all use `dependencies: HashSet<String>` as their backing
    // store). Consolidated via the `impl_dependencies_accessors!()`
    // macro in `changepacks-core` so future accessor tweaks land in
    // one place — expansion is byte-identical to the previous
    // hand-rolled bodies.
    changepacks_core::impl_dependencies_accessors!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{captured_argv, create_publish_wrapper};
    use changepacks_core::UpdateType;
    use rstest::rstest;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    fn assert_gradle_workspace_defaults(workspace: &GradleWorkspace) {
        assert_eq!(workspace.name(), Some("test-workspace"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), PathBuf::from("/test/build.gradle.kts"));
        assert_eq!(
            workspace.relative_path(),
            PathBuf::from("test/build.gradle.kts")
        );
        assert_eq!(workspace.language(), Language::Java);
        assert!(!workspace.is_changed());
        assert!(workspace.is_publishable_by_default());
        assert!(workspace.is_dry_run_publishable_by_default());
        #[cfg(windows)]
        {
            assert_eq!(
                workspace.default_publish_command(),
                ".\\gradlew.bat publish"
            );
            assert_eq!(
                workspace.default_dry_run_publish_command().as_deref(),
                Some(".\\gradlew.bat publishToMavenLocal")
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(workspace.default_publish_command(), "./gradlew publish");
            assert_eq!(
                workspace.default_dry_run_publish_command().as_deref(),
                Some("./gradlew publishToMavenLocal")
            );
        }
    }

    #[tokio::test]
    async fn test_gradle_workspace_new() {
        let workspace = GradleWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        );

        assert_gradle_workspace_defaults(&workspace);
    }

    #[test]
    fn test_gradle_workspace_publishability_tracks_available_tasks() {
        let remote_only = GradleWorkspace::new_with_publish_tasks(
            Some("remote-only".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
            true,
            false,
        );
        let local_only = GradleWorkspace::new_with_publish_tasks(
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

    #[test]
    fn test_gradle_workspace_set_changed() {
        changepacks_core::assert_set_changed_roundtrip!(GradleWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("test/build.gradle.kts"),
        ));
    }

    #[tokio::test]
    async fn test_publish_root_project_uses_platform_wrapper_publish_task() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path().join("repo with spaces");
        fs::create_dir_all(&root).unwrap();
        create_publish_wrapper(&root);
        let manifest = root.join("build.gradle.kts");
        fs::write(&manifest, "version = \"1.0.0\"\n").unwrap();
        let workspace = GradleWorkspace::new(
            Some("root".to_string()),
            Some("1.0.0".to_string()),
            manifest,
            PathBuf::from("build.gradle.kts"),
        );

        let output = workspace
            .publish(&changepacks_core::Config::default())
            .await
            .unwrap();
        let dry_run = workspace
            .dry_run_publish(&changepacks_core::Config::default())
            .await
            .unwrap()
            .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains(&format!("cwd={}", root.display())));
        assert_eq!(captured_argv(&output.stdout), ["publish"]);
        assert!(dry_run.success, "stderr: {}", dry_run.stderr);
        let dry_run_argv = captured_argv(&dry_run.stdout);
        assert_eq!(dry_run_argv.len(), 2, "stdout: {}", dry_run.stdout);
        assert_eq!(dry_run_argv[0], "publishToMavenLocal");
        assert!(dry_run_argv[1].starts_with("-Dmaven.repo.local="));
    }

    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_gradle_workspace_update_version_kts(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
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

        workspace.update_version(update_type).await.unwrap();

        let content = read_to_string(&build_gradle).await.unwrap();
        assert!(content.contains(&format!(r#"version = "{expected}""#)));
        assert_eq!(workspace.version(), Some(expected));

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
            r"
plugins {
    id 'java'
}

group = 'com.example'
version = '1.0.0'
",
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
        assert_eq!(workspace.version(), Some("1.0.1"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn gradle_workspace_updates_groovy_version_from_sibling_properties() {
        let temp_dir = TempDir::new().unwrap();
        let build_gradle = temp_dir.path().join("build.gradle");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"plugins { id 'java' }\n";
        let properties_content = b"version : 2.0.0 ! workspace\n";
        tokio::fs::write(&build_gradle, build_content)
            .await
            .unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();
        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            Some("2.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("build.gradle"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        assert_eq!(workspace.version(), Some("2.0.1"));
        assert_eq!(tokio::fs::read(&build_gradle).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            b"version : 2.0.1 ! workspace\n"
        );
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

    #[tokio::test]
    async fn test_gradle_workspace_update_version_errors_for_unsupported_declaration() {
        let temp_dir = TempDir::new().unwrap();
        let build_gradle = temp_dir.path().join("build.gradle.kts");
        let content = "version = providers.gradleProperty(\"releaseVersion\").get()\n";
        fs::write(&build_gradle, content).unwrap();
        let bytes_before = fs::read(&build_gradle).unwrap();
        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("build.gradle.kts"),
        );

        let result = workspace.update_version(UpdateType::Patch).await;

        assert!(result.is_err());
        assert_eq!(fs::read(&build_gradle).unwrap(), bytes_before);
        assert_eq!(workspace.version(), Some("1.0.0"));
    }

    #[tokio::test]
    async fn test_gradle_workspace_ambiguous_version_keeps_file_and_state_unchanged() {
        let temp_dir = TempDir::new().unwrap();
        let build_gradle = temp_dir.path().join("build.gradle");
        let content = "version = '1.0.0'\nallprojects {\n    version = 'duplicate'\n}\n";
        fs::write(&build_gradle, content).unwrap();
        let bytes_before = fs::read(&build_gradle).unwrap();
        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("build.gradle"),
        );

        let result = workspace.update_version(UpdateType::Patch).await;

        assert!(result.is_err());
        assert_eq!(fs::read(&build_gradle).unwrap(), bytes_before);
        assert_eq!(workspace.version(), Some("1.0.0"));
    }

    #[tokio::test]
    async fn test_gradle_workspace_updates_allprojects_version_with_exact_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let build_gradle = temp_dir.path().join("build.gradle");
        let content = "plugins {\r\n    version = 'plugin-version'\r\n}\r\nallprojects {\r\n\tversion  =  \"1.0.0\" // project-wide\r\n}\r\n";
        fs::write(&build_gradle, content).unwrap();
        let mut workspace = GradleWorkspace::new(
            Some("multiproject".to_string()),
            Some("1.0.0".to_string()),
            build_gradle.clone(),
            PathBuf::from("build.gradle"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        assert_eq!(
            fs::read_to_string(&build_gradle).unwrap(),
            "plugins {\r\n    version = 'plugin-version'\r\n}\r\nallprojects {\r\n\tversion  =  \"1.0.1\" // project-wide\r\n}\r\n"
        );
        assert_eq!(workspace.version(), Some("1.0.1"));
    }

    #[test]
    fn test_gradle_workspace_dependencies() {
        changepacks_core::assert_dependencies_roundtrip!(
            GradleWorkspace::new(
                Some("test-workspace".to_string()),
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
        changepacks_core::assert_set_name_roundtrip!(GradleWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/build.gradle.kts"),
            PathBuf::from("build.gradle.kts"),
        ));
    }
}
