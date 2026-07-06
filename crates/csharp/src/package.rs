use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::publish::PublishOutput;
use changepacks_core::{Config, Language, Package, UpdateType};

use crate::dry_run::resolve_and_run_dry_run;

#[derive(Debug)]
pub struct CSharpPackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl CSharpPackage {
    // Standard package/workspace constructor.
    changepacks_core::impl_default_new!();
}

#[async_trait]
impl Package for CSharpPackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // `update_version` shares its byte-identical body with `CSharpWorkspace`.
    // Consolidated via the shared `update_version_from_fields` helper in
    // `crates/csharp/src/lib.rs` so the "reserve `0.0.0`" fallback and the
    // `has_version` derivation live in ONE place. See the helper's doc
    // comment for why a `macro_rules!` producing `async fn` is
    // incompatible with `#[async_trait]` (E0195 lifetime mismatch).
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::update_version_from_fields(&mut self.version, &self.path, update_type).await
    }

    // Fixed language accessor.
    changepacks_core::impl_language!(Language::CSharp);

    // `default_publish_command` returns the const from `crate` (see
    // `lib.rs`). `default_dry_run_publish_command` returns `None` because
    // no single shell one-liner reliably represents the C# dry-run flow
    // (pack + push to an ephemeral local feed + guaranteed cleanup);
    // returning `None` still lets users supply a custom shell command via
    // `publishDryRun` in config. The actual managed RAII dry-run flow
    // lives in `dry_run_publish` below (delegates to
    // `dry_run::resolve_and_run_dry_run`, which honors the `publishDryRun`
    // override first).
    changepacks_core::impl_const_publish_commands!(crate::PUBLISH_COMMAND);

    /// Managed dry-run for C#/.NET packages.
    ///
    /// Honors `config.publishDryRun` overrides first (existing shell-string
    /// behavior, matching every other language). When no override is set,
    /// runs `dotnet pack` + `dotnet nuget push` against ephemeral
    /// `tempfile::TempDir` directories that are cleaned up via RAII — even
    /// on error, panic, or future cancellation.
    #[cfg(not(tarpaulin_include))]
    async fn dry_run_publish(&self, config: &Config) -> Result<Option<PublishOutput>> {
        resolve_and_run_dry_run(
            self.path(),
            self.relative_path(),
            config,
            "Package directory not found",
        )
        .await
    }

    // Dependency set accessors.
    changepacks_core::impl_dependencies_accessors!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_new() {
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
        assert_eq!(
            package.default_publish_command(),
            "dotnet pack -c Release && dotnet nuget push"
        );
        // `dotnet nuget push` has no built-in dry-run mode, so the crate
        // returns None and lets the publish loop skip with a warning.
        assert!(package.default_dry_run_publish_command().is_none());

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_set_changed() {
        let mut package = CSharpPackage::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("Test.csproj"),
        );

        assert!(!package.is_changed());
        package.set_changed(true);
        assert!(package.is_changed());
        package.set_changed(false);
        assert!(!package.is_changed());
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

    #[test]
    fn test_dependencies() {
        let mut package = CSharpPackage::new(
            Some("Test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("test/Test.csproj"),
        );

        // Initially empty
        assert!(package.dependencies().is_empty());

        // Add dependencies
        package.add_dependency("Newtonsoft.Json");
        package.add_dependency("CoreLib");

        let deps = package.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("Newtonsoft.Json"));
        assert!(deps.contains("CoreLib"));

        // Adding duplicate should not increase count
        package.add_dependency("Newtonsoft.Json");
        assert_eq!(package.dependencies().len(), 2);
    }

    #[test]
    fn test_set_name() {
        let mut package = CSharpPackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Test.csproj"),
            PathBuf::from("Test.csproj"),
        );
        assert_eq!(package.name(), None);
        package.set_name("my-project".to_string());
        assert_eq!(package.name(), Some("my-project"));
    }
}
