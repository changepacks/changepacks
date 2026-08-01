use std::{ffi::OsString, future::Future, path::PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::publish::PublishOutput;
use changepacks_core::{Config, Language, Package, UpdateType};

use crate::dry_run::{
    resolve_and_run_dry_run_with_command_runner, resolve_and_run_publish_with_command_runner,
    run_dotnet_command,
};

// Seven-field discovered-project declaration plus `new` / `new_discovered`,
// shared verbatim with the other four identical language types. The
// command-runner helpers below stay in their own inherent impl block.
changepacks_core::declare_discovered_project!(pub struct CSharpPackage);

impl CSharpPackage {
    /// Real publish with an injected command boundary, so tests can drive the
    /// managed `dotnet pack` + `dotnet nuget push` flow without spawning
    /// processes.
    async fn publish_with_command_runner<F, Fut>(
        &self,
        config: &Config,
        runner: F,
    ) -> Result<PublishOutput>
    where
        F: FnMut(&'static str, Vec<OsString>, PathBuf) -> Fut,
        Fut: Future<Output = Result<PublishOutput>>,
    {
        resolve_and_run_publish_with_command_runner(
            self.path(),
            self.relative_path(),
            config,
            changepacks_core::publish::PACKAGE_DIR_NOT_FOUND,
            runner,
        )
        .await
    }

    /// Dry-run publish with an injected command boundary, mirroring
    /// [`Self::publish_with_command_runner`] against the temporary local feed.
    async fn dry_run_publish_with_command_runner<F, Fut>(
        &self,
        config: &Config,
        runner: F,
    ) -> Result<Option<PublishOutput>>
    where
        F: FnMut(&'static str, Vec<OsString>, PathBuf) -> Fut,
        Fut: Future<Output = Result<PublishOutput>>,
    {
        resolve_and_run_dry_run_with_command_runner(
            self.path(),
            self.relative_path(),
            config,
            changepacks_core::publish::PACKAGE_DIR_NOT_FOUND,
            runner,
        )
        .await
    }
}

#[async_trait]
impl Package for CSharpPackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // Publishability flag accessor.
    changepacks_core::impl_publishable_by_default!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let path = &self.path;
        changepacks_utils::bump_version_with(&mut self.version, path, update_type, async |new| {
            crate::write_csproj_version(path, new).await
        })
        .await
    }

    // Fixed language accessor.
    changepacks_core::impl_language!(Language::CSharp);

    // The legacy accessor value remains stable, but `publish` below never
    // executes it: real and dry-run publishing use managed argv flows after
    // resolving path/language overrides. Dry-run's default command remains
    // `None` because no single shell command can safely model its local feed.
    changepacks_core::impl_const_publish_commands!(crate::PUBLISH_COMMAND);

    async fn publish(&self, config: &Config) -> Result<PublishOutput> {
        self.publish_with_command_runner(config, run_dotnet_command)
            .await
    }

    /// Managed dry-run for C#/.NET packages.
    ///
    /// Honors `config.publishDryRun` overrides first (existing shell-string
    /// behavior, matching every other language). When no override is set,
    /// runs `dotnet pack` + `dotnet nuget push` against ephemeral
    /// `tempfile::TempDir` directories that are cleaned up via RAII — even
    /// on error, panic, or future cancellation.
    async fn dry_run_publish(&self, config: &Config) -> Result<Option<PublishOutput>> {
        self.dry_run_publish_with_command_runner(config, run_dotnet_command)
            .await
    }

    // Dependency set accessors.
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

    #[test]
    fn test_new() {
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

        let package = CSharpPackage::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            csproj_path.clone(),
            PathBuf::from("Test.csproj"),
        );

        assert_eq!(package.name(), Some("Test"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), csproj_path);
        assert_eq!(package.relative_path(), PathBuf::from("Test.csproj"));
        assert!(!package.is_changed());
        assert_eq!(package.language(), Language::CSharp);
        assert!(package.is_publishable_by_default());
        assert_eq!(
            package.default_publish_command(),
            "dotnet pack -c Release && dotnet nuget push"
        );
        // The legacy command accessor remains `None`; the overridden
        // `dry_run_publish` method supplies the managed temporary-feed flow.
        assert!(package.default_dry_run_publish_command().is_none());

        temp_dir.close().unwrap();
    }

    #[rstest]
    #[case(true)]
    #[case(false)]
    fn test_csharp_package_discovered_publishability(#[case] expected: bool) {
        let package = CSharpPackage::new_discovered(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("Test.csproj"),
            expected,
        );

        assert_eq!(package.is_publishable_by_default(), expected);
    }

    #[tokio::test]
    async fn test_dry_run_publish_forwards_path_override_without_dotnet() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        let relative_path = PathBuf::from("packages/Test.csproj");
        let package = CSharpPackage::new(None, None, csproj_path, relative_path.clone());
        let mut config = Config::default();
        config.publish_dry_run.insert(
            relative_path.to_string_lossy().into_owned(),
            "echo package-forwarded".to_string(),
        );

        let output = package.dry_run_publish(&config).await.unwrap().unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("package-forwarded"));
    }

    #[tokio::test]
    async fn test_publish_forwards_path_override_without_managed_dotnet_flow() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        let relative_path = PathBuf::from("packages/Test.csproj");
        let package = CSharpPackage::new(None, None, csproj_path, relative_path.clone());
        let mut config = Config::default();
        config.publish.insert(
            relative_path.to_string_lossy().into_owned(),
            "echo package-publish-forwarded".to_string(),
        );

        let output = package.publish(&config).await.unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output.stdout.contains("package-publish-forwarded"));
    }

    #[tokio::test]
    async fn test_publish_and_dry_run_preserve_package_directory_error_message() {
        let root = if cfg!(target_os = "windows") {
            PathBuf::from(r"C:\")
        } else {
            PathBuf::from("/")
        };
        let package = CSharpPackage::new(None, None, root, PathBuf::from("Test.csproj"));

        let publish_error = package.publish(&Config::default()).await.unwrap_err();
        let dry_run_error = package
            .dry_run_publish(&Config::default())
            .await
            .unwrap_err();

        assert_eq!(
            publish_error.to_string(),
            changepacks_core::publish::PACKAGE_DIR_NOT_FOUND
        );
        assert_eq!(
            dry_run_error.to_string(),
            changepacks_core::publish::PACKAGE_DIR_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn test_managed_publish_default_through_package_surfaces_cleanup_message() {
        let temp_dir = TempDir::new().unwrap();
        let package = CSharpPackage::new(
            None,
            None,
            temp_dir.path().join("Test.csproj"),
            PathBuf::from("Test.csproj"),
        );
        let pack_path = Arc::new(Mutex::new(None::<PathBuf>));
        let recorded_pack_path = Arc::clone(&pack_path);

        let output = package
            .publish_with_command_runner(&Config::default(), move |_program, args, _working_dir| {
                let recorded_pack_path = Arc::clone(&recorded_pack_path);
                async move {
                    let is_pack = args.first().and_then(|arg| arg.to_str()) == Some("pack");
                    if is_pack {
                        let path = PathBuf::from(&args[5]);
                        assert_eq!(
                            args,
                            vec![
                                OsString::from("pack"),
                                OsString::from("Test.csproj"),
                                OsString::from("-c"),
                                OsString::from("Release"),
                                OsString::from("-o"),
                                path.clone().into_os_string(),
                            ]
                        );
                        fs::write(path.join("only.nupkg"), b"").unwrap();
                        *recorded_pack_path.lock().unwrap() = Some(path);
                    } else {
                        assert_eq!(
                            args,
                            vec![
                                OsString::from("nuget"),
                                OsString::from("push"),
                                PathBuf::from(&args[2]).into_os_string(),
                                OsString::from("--skip-duplicate"),
                            ]
                        );
                        let path = PathBuf::from(&args[2]).parent().unwrap().to_path_buf();
                        fs::remove_dir_all(&path).unwrap();
                        fs::write(&path, b"force cleanup error").unwrap();
                    }
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
        assert!(
            output
                .stderr
                .contains("[changepacks publish] pack tempdir cleanup error:")
        );
        let pack_path = pack_path.lock().unwrap().take().unwrap();
        assert!(pack_path.is_file());
        fs::remove_file(pack_path).unwrap();
    }

    #[tokio::test]
    async fn test_managed_dry_run_default_through_package_uses_temporary_feed() {
        let temp_dir = TempDir::new().unwrap();
        let package = CSharpPackage::new(
            None,
            None,
            temp_dir.path().join("Test.csproj"),
            PathBuf::from("Test.csproj"),
        );
        let calls = Arc::new(Mutex::new(Vec::<Vec<OsString>>::new()));
        let recorded_calls = Arc::clone(&calls);

        let output = package
            .dry_run_publish_with_command_runner(
                &Config::default(),
                move |_program, args, _working_dir| {
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
                },
            )
            .await
            .unwrap()
            .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "calls: {calls:?}");
        assert_eq!(calls[1][3], "-s");
        let pack_dir = PathBuf::from(&calls[0][5]);
        assert_eq!(
            calls[0],
            vec![
                OsString::from("pack"),
                OsString::from("Test.csproj"),
                OsString::from("-c"),
                OsString::from("Release"),
                OsString::from("-o"),
                pack_dir.clone().into_os_string(),
            ]
        );
        let feed_dir = PathBuf::from(&calls[1][4]);
        assert!(!pack_dir.exists());
        assert!(!feed_dir.exists());
    }

    #[test]
    fn test_set_changed() {
        changepacks_core::assert_set_changed_roundtrip!(CSharpPackage::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("Test.csproj"),
        ));
    }

    // Patch, Minor, and Major all share the same setup (write a csproj with
    // `<Version>1.0.0</Version>`, construct the package, call
    // `update_version`, read back); only the bump kind and the expected
    // resulting version string differ.
    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_update_version(#[case] update_type: UpdateType, #[case] expected_version: &str) {
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

        let mut package = CSharpPackage::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            csproj_path.clone(),
            PathBuf::from("Test.csproj"),
        );

        package.update_version(update_type).await.unwrap();

        let content = fs::read_to_string(&csproj_path).unwrap();
        assert!(content.contains(&format!("<Version>{expected_version}</Version>")));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_preserves_other_elements() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Test.csproj");
        let original_content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net8.0</TargetFramework>
    <Version>1.0.0</Version>
    <PackageId>MyPackage</PackageId>
  </PropertyGroup>
</Project>
"#;
        fs::write(&csproj_path, original_content).unwrap();

        let mut package = CSharpPackage::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            csproj_path.clone(),
            PathBuf::from("Test.csproj"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&csproj_path).unwrap();
        assert!(content.contains("<Version>1.0.1</Version>"));
        assert!(content.contains("<OutputType>Exe</OutputType>"));
        assert!(content.contains("<TargetFramework>net8.0</TargetFramework>"));
        assert!(content.contains("<PackageId>MyPackage</PackageId>"));

        temp_dir.close().unwrap();
    }

    /// The `Package` trait entry point — not just the `write_csproj_version`
    /// helper — must reject a malformed `.csproj` without partially writing it,
    /// matching the Node/Python/Dart siblings. C# cannot use the shared
    /// `changepacks_utils::assert_malformed_manifest_rejected!` macro because
    /// its context is `Failed to update version in C# project {path}` rather
    /// than the `Failed to parse {label}` template, so the assertions are
    /// written out here. Pinning that exact context (rather than just the path)
    /// also proves the failure comes from the XML update leg and not the read
    /// leg, which would name the same path under a different message.
    #[tokio::test]
    async fn test_csharp_package_update_version_malformed_manifest_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Broken.csproj");
        // Unclosed `</PropertyGroup` and a missing `</Project>` make the
        // manifest unparseable, so the version bump must fail before any
        // write reaches disk.
        let original_bytes =
            b"<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version>1.0.0</Version>\n  </PropertyGroup\n";
        fs::write(&csproj_path, original_bytes).unwrap();

        let mut package = CSharpPackage::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            csproj_path.clone(),
            PathBuf::from("Broken.csproj"),
        );

        let error = package
            .update_version(UpdateType::Patch)
            .await
            .expect_err("a malformed .csproj must fail the version bump");
        let chain = format!("{error:#}");

        assert!(
            chain.contains(&format!(
                "Failed to update version in C# project {}",
                csproj_path.display()
            )),
            "error chain should carry the update context naming the manifest path, got: {chain}"
        );
        assert_eq!(
            fs::read(&csproj_path).unwrap(),
            original_bytes,
            "a failed bump must leave the manifest bytes untouched"
        );
        assert_eq!(
            package.version(),
            Some("1.0.0"),
            "a failed bump must leave the in-memory version untouched"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_without_property_group_creates_global_version() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("NoPropertyGroup.csproj");
        let original_content = b"<Project Sdk=\"Microsoft.NET.Sdk\">\r\n</Project>\r\n";
        fs::write(&csproj_path, original_content).unwrap();
        let mut package = CSharpPackage::new(
            Some("Test".to_string()),
            None,
            csproj_path.clone(),
            PathBuf::from("NoPropertyGroup.csproj"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        assert_eq!(
            fs::read_to_string(&csproj_path).unwrap(),
            "<Project Sdk=\"Microsoft.NET.Sdk\">\r\n<PropertyGroup>\r\n    <Version>0.0.1</Version>\r\n</PropertyGroup>\r\n</Project>\r\n"
        );
        assert_eq!(package.version(), Some("0.0.1"));
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_with_stale_metadata_ignores_conditional_version() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("StaleVersion.csproj");
        let original_content = b"<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version>1.2.3</Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n  </PropertyGroup>\n</Project>\n";
        fs::write(&csproj_path, original_content).unwrap();
        let mut package = CSharpPackage::new(
            Some("Test".to_string()),
            Some("1.2.3".to_string()),
            csproj_path.clone(),
            PathBuf::from("StaleVersion.csproj"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        assert_eq!(
            fs::read_to_string(&csproj_path).unwrap(),
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version>1.2.3</Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <Version>1.2.4</Version>\n  </PropertyGroup>\n</Project>\n"
        );
        assert_eq!(package.version(), Some("1.2.4"));
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_dependencies() {
        changepacks_core::assert_dependencies_roundtrip!(
            CSharpPackage::new(
                Some("Test".to_string()),
                Some("1.0.0".to_string()),
                PathBuf::from("/test/Test.csproj"),
                PathBuf::from("test/Test.csproj"),
            ),
            "Newtonsoft.Json",
            "CoreLib"
        );
    }

    #[test]
    fn test_set_name() {
        changepacks_core::assert_set_name_roundtrip!(CSharpPackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("Test.csproj"),
        ));
    }
}
