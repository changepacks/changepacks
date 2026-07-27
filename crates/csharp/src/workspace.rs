use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::publish::PublishOutput;
use changepacks_core::{Config, Language, UpdateType, Workspace};
use std::{collections::HashSet, path::PathBuf};

use crate::dry_run::run_dotnet_command;

#[derive(Debug)]
pub struct CSharpWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl CSharpWorkspace {
    // Byte-identical `#[must_use] pub fn new(name, version, path,
    // relative_path)` constructor body shared with every other
    // "plain 5-basic-field" language crate's `Package` / `Workspace`.
    // Consolidated via `impl_default_new!()` in `changepacks-core` — see
    // that macro's doc for the exact struct-field contract.
    changepacks_core::impl_default_new!();

    // Same byte-identical command-runner wrapper pair as `CSharpPackage`,
    // differing only in the missing-parent-directory message. See
    // `crate::dry_run::impl_csharp_command_runner_wrappers!`.
    crate::dry_run::impl_csharp_command_runner_wrappers!(
        changepacks_core::publish::WORKSPACE_DIR_NOT_FOUND
    );
}

#[async_trait]
impl Workspace for CSharpWorkspace {
    // Seven basic accessors (`name`, `version`, `path`, `relative_path`,
    // `is_changed`, `set_changed`, `set_name`) share their byte-identical
    // bodies with every other language crate's `Package` / `Workspace`
    // impl. Consolidated via `impl_basic_accessors!()` in `changepacks-core`
    // — expansion is byte-identical to the previous hand-rolled bodies.
    changepacks_core::impl_basic_accessors!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let path = &self.path;
        changepacks_utils::bump_version_with(&mut self.version, path, update_type, async |new| {
            crate::write_csproj_version(path, new).await
        })
        .await
    }

    // Byte-identical `fn language(&self) -> Language { Language::CSharp }`
    // one-liner shared with every other language crate's `Package` /
    // `Workspace` impl. Consolidated via `impl_language!()` in
    // `changepacks-core` alongside the other accessor macros.
    changepacks_core::impl_language!(Language::CSharp);

    // Keep the legacy accessor values aligned with `CSharpPackage`; the
    // `publish` and `dry_run_publish` overrides below perform the actual
    // managed flows after honoring configuration overrides.
    changepacks_core::impl_const_publish_commands!(crate::PUBLISH_COMMAND);

    async fn publish(&self, config: &Config) -> Result<PublishOutput> {
        self.publish_with_command_runner(config, run_dotnet_command)
            .await
    }

    /// Managed dry-run for C#/.NET workspaces. See [`crate::package::CSharpPackage::dry_run_publish`]
    /// for the full rationale — workspace and package share identical
    /// semantics here.
    async fn dry_run_publish(&self, config: &Config) -> Result<Option<PublishOutput>> {
        self.dry_run_publish_with_command_runner(config, run_dotnet_command)
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
    use rstest::rstest;
    use std::{
        ffi::OsString,
        fs,
        sync::{Arc, Mutex},
    };
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_new_with_name_and_version() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        fs::write(
            &csproj_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();

        let workspace = CSharpWorkspace::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            csproj_path.clone(),
            PathBuf::from("Test.csproj"),
        );

        assert_eq!(workspace.name(), Some("Test"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), csproj_path);
        assert_eq!(workspace.relative_path(), PathBuf::from("Test.csproj"));
        assert!(!workspace.is_changed());
        assert_eq!(workspace.language(), Language::CSharp);
        assert_eq!(
            workspace.default_publish_command(),
            "dotnet pack -c Release && dotnet nuget push"
        );
        // `dotnet nuget push` has no built-in dry-run mode.
        assert!(workspace.default_dry_run_publish_command().is_none());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_new_without_name_and_version() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        fs::write(
            &csproj_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();

        let workspace = CSharpWorkspace::new(
            None,
            None,
            csproj_path.clone(),
            PathBuf::from("Test.csproj"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
        assert_eq!(workspace.path(), csproj_path);
        assert!(!workspace.is_changed());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_dry_run_publish_forwards_language_override_without_dotnet() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        let workspace = CSharpWorkspace::new(
            None,
            None,
            csproj_path,
            PathBuf::from("workspaces/Test.csproj"),
        );
        let mut config = Config::default();
        config
            .publish_dry_run
            .insert("csharp".to_string(), "echo workspace-forwarded".to_string());

        let output = workspace.dry_run_publish(&config).await.unwrap().unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("workspace-forwarded"));
    }

    #[tokio::test]
    async fn test_publish_forwards_language_override_without_managed_dotnet_flow() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        let workspace = CSharpWorkspace::new(
            None,
            None,
            csproj_path,
            PathBuf::from("workspaces/Test.csproj"),
        );
        let mut config = Config::default();
        config.publish.insert(
            "csharp".to_string(),
            "echo workspace-publish-forwarded".to_string(),
        );

        let output = workspace.publish(&config).await.unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("workspace-publish-forwarded"));
    }

    #[tokio::test]
    async fn test_publish_and_dry_run_preserve_workspace_directory_error_message() {
        let root = if cfg!(target_os = "windows") {
            PathBuf::from(r"C:\")
        } else {
            PathBuf::from("/")
        };
        let workspace = CSharpWorkspace::new(None, None, root, PathBuf::from("Test.csproj"));

        let publish_error = workspace.publish(&Config::default()).await.unwrap_err();
        let dry_run_error = workspace
            .dry_run_publish(&Config::default())
            .await
            .unwrap_err();

        assert_eq!(
            publish_error.to_string(),
            changepacks_core::publish::WORKSPACE_DIR_NOT_FOUND
        );
        assert_eq!(
            dry_run_error.to_string(),
            changepacks_core::publish::WORKSPACE_DIR_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn test_managed_publish_default_through_workspace_uses_user_nuget_config() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = CSharpWorkspace::new(
            None,
            None,
            temp_dir.path().join("Test.csproj"),
            PathBuf::from("Test.csproj"),
        );
        let calls = Arc::new(Mutex::new(Vec::<Vec<OsString>>::new()));
        let recorded_calls = Arc::clone(&calls);

        let output = workspace
            .publish_with_command_runner(&Config::default(), move |_program, args, _working_dir| {
                let recorded_calls = Arc::clone(&recorded_calls);
                async move {
                    if args.first().and_then(|arg| arg.to_str()) == Some("pack") {
                        let pack_dir = PathBuf::from(&args[5]);
                        fs::write(pack_dir.join("only.nupkg"), b"").unwrap();
                    }
                    recorded_calls.lock().unwrap().push(args);
                    Ok(PublishOutput {
                        success: true,
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                }
            })
            .await
            .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "calls: {calls:?}");
        let pack_dir = PathBuf::from(&calls[0][5]);
        assert_eq!(
            calls[0],
            vec![
                OsString::from("pack"),
                OsString::from("Test.csproj"),
                OsString::from("-c"),
                OsString::from("Release"),
                OsString::from("-o"),
                pack_dir.into_os_string(),
            ]
        );
        assert_eq!(
            calls[1],
            vec![
                OsString::from("nuget"),
                OsString::from("push"),
                PathBuf::from(&calls[1][2]).into_os_string(),
                OsString::from("--skip-duplicate"),
            ]
        );
    }

    #[tokio::test]
    async fn test_managed_dry_run_default_through_workspace_surfaces_cleanup_message() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = CSharpWorkspace::new(
            None,
            None,
            temp_dir.path().join("Test.csproj"),
            PathBuf::from("Test.csproj"),
        );
        let feed_path = Arc::new(Mutex::new(None::<PathBuf>));
        let recorded_feed_path = Arc::clone(&feed_path);

        let output = workspace
            .dry_run_publish_with_command_runner(
                &Config::default(),
                move |_program, args, _working_dir| {
                    let recorded_feed_path = Arc::clone(&recorded_feed_path);
                    async move {
                        let is_pack = args.first().and_then(|arg| arg.to_str()) == Some("pack");
                        if is_pack {
                            let pack_dir = PathBuf::from(&args[5]);
                            assert_eq!(
                                args,
                                vec![
                                    OsString::from("pack"),
                                    OsString::from("Test.csproj"),
                                    OsString::from("-c"),
                                    OsString::from("Release"),
                                    OsString::from("-o"),
                                    pack_dir.clone().into_os_string(),
                                ]
                            );
                            fs::write(pack_dir.join("only.nupkg"), b"").unwrap();
                        } else {
                            assert_eq!(args[3], "-s");
                            let path = PathBuf::from(&args[4]);
                            fs::remove_dir_all(&path).unwrap();
                            fs::write(&path, b"force cleanup error").unwrap();
                            *recorded_feed_path.lock().unwrap() = Some(path);
                        }
                        Ok(PublishOutput {
                            success: true,
                            stdout: String::new(),
                            stderr: String::new(),
                        })
                    }
                },
            )
            .await
            .unwrap()
            .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(
            output
                .stderr
                .contains("[changepacks dry-run] feed tempdir cleanup error:")
        );
        let feed_path = feed_path.lock().unwrap().take().unwrap();
        assert!(feed_path.is_file());
        fs::remove_file(feed_path).unwrap();
    }

    #[test]
    fn test_set_changed() {
        let mut workspace = CSharpWorkspace::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("Test.csproj"),
        );

        assert!(!workspace.is_changed());
        workspace.set_changed(true);
        assert!(workspace.is_changed());
        workspace.set_changed(false);
        assert!(!workspace.is_changed());
    }

    // Patch (with existing version), Minor, and Major all share the same
    // setup: a csproj carrying `<Version>1.0.0</Version>`, a workspace
    // constructed with `Some("1.0.0")`. Only the bump kind and the
    // expected resulting version string differ. The `None`-version case
    // (`test_update_version_without_version`) stays separate below because
    // it uses a different csproj fixture and constructor.
    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_update_version_with_existing_version(
        #[case] update_type: UpdateType,
        #[case] expected_version: &str,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        fs::write(
            &csproj_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();

        let mut workspace = CSharpWorkspace::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            csproj_path.clone(),
            PathBuf::from("Test.csproj"),
        );

        workspace.update_version(update_type).await.unwrap();

        let content = fs::read_to_string(&csproj_path).unwrap();
        assert!(content.contains(&format!("<Version>{expected_version}</Version>")));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        fs::write(
            &csproj_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();

        let mut workspace = CSharpWorkspace::new(
            Some("Test".to_string()),
            None,
            csproj_path.clone(),
            PathBuf::from("Test.csproj"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&csproj_path).unwrap();
        assert!(content.contains("<Version>0.0.1</Version>"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_ignores_version_outside_property_group() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("UnsupportedVersion.csproj");
        let original_content = b"<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Version>1.2.3</Version>\n  </ItemGroup>\n</Project>\n";
        fs::write(&csproj_path, original_content).unwrap();
        let mut workspace = CSharpWorkspace::new(
            Some("Test".to_string()),
            Some("1.2.3".to_string()),
            csproj_path.clone(),
            PathBuf::from("UnsupportedVersion.csproj"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        assert_eq!(
            fs::read_to_string(&csproj_path).unwrap(),
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Version>1.2.3</Version>\n  </ItemGroup>\n  <PropertyGroup>\n    <Version>1.2.4</Version>\n  </PropertyGroup>\n</Project>\n"
        );
        assert_eq!(workspace.version(), Some("1.2.4"));
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_dependencies() {
        let mut workspace = CSharpWorkspace::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("test/Test.csproj"),
        );

        // Initially empty
        assert!(workspace.dependencies().is_empty());

        // Add dependencies
        workspace.add_dependency("Newtonsoft.Json");
        workspace.add_dependency("CoreLib");

        let deps = workspace.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("Newtonsoft.Json"));
        assert!(deps.contains("CoreLib"));

        // Adding duplicate should not increase count
        workspace.add_dependency("Newtonsoft.Json");
        assert_eq!(workspace.dependencies().len(), 2);
    }

    #[test]
    fn test_set_name() {
        let mut workspace = CSharpWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("Test.csproj"),
        );
        assert_eq!(workspace.name(), None);
        workspace.set_name("my-project".to_string());
        assert_eq!(workspace.name(), Some("my-project"));
    }
}
