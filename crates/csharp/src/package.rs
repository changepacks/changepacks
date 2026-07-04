use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::publish::PublishOutput;
use changepacks_core::{Config, Language, Package, UpdateType};
use changepacks_utils::next_version;

use crate::dry_run::resolve_and_run_dry_run;
use crate::write_csproj_version;

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
    #[must_use]
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
            dependencies: HashSet::new(),
        }
    }
}

#[async_trait]
impl Package for CSharpPackage {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let current_version = self.version.as_deref().unwrap_or("0.0.0");
        let new_version = next_version(current_version, update_type)?;
        write_csproj_version(&self.path, &new_version, self.version.is_some()).await?;
        self.version = Some(new_version);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::CSharp
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    fn default_publish_command(&self) -> String {
        crate::PUBLISH_COMMAND.to_string()
    }

    fn default_dry_run_publish_command(&self) -> Option<String> {
        // No single shell one-liner reliably represents the C# dry-run flow
        // (pack + push to an ephemeral local feed + guaranteed cleanup), so
        // we return `None` here and override `dry_run_publish` below with a
        // managed RAII implementation. Returning `None` still lets users
        // supply a custom shell command via `publishDryRun` in config; that
        // override is honored first inside the override.
        None
    }

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

    #[tokio::test]
    async fn test_set_changed() {
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

        assert!(!package.is_changed());
        package.set_changed(true);
        assert!(package.is_changed());
        package.set_changed(false);
        assert!(!package.is_changed());

        temp_dir.close().unwrap();
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
