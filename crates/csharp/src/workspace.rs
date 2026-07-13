use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::publish::PublishOutput;
use changepacks_core::{Config, Language, UpdateType, Workspace};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::dry_run::resolve_and_run_dry_run;

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
        let has_version = self.version.is_some();
        changepacks_utils::bump_version_with(&mut self.version, path, update_type, async |new| {
            crate::write_csproj_version(path, new, has_version).await
        })
        .await
    }

    // Byte-identical `fn language(&self) -> Language { Language::CSharp }`
    // one-liner shared with every other language crate's `Package` /
    // `Workspace` impl. Consolidated via `impl_language!()` in
    // `changepacks-core` alongside the other accessor macros.
    changepacks_core::impl_language!(Language::CSharp);

    // `default_publish_command` returns the const from `crate` (see
    // `lib.rs`). `default_dry_run_publish_command` returns `None` because
    // no single shell one-liner reliably represents the C# dry-run flow.
    // See `CSharpPackage::default_dry_run_publish_command` for full
    // rationale. The actual dry-run logic lives in the `dry_run_publish`
    // override below.
    //
    // Consolidated via the single-arg form of
    // `impl_const_publish_commands!()` in `changepacks-core` — expansion
    // is byte-identical to the previous hand-rolled bodies.
    changepacks_core::impl_const_publish_commands!(crate::PUBLISH_COMMAND);

    /// Managed dry-run for C#/.NET workspaces. See [`crate::package::CSharpPackage::dry_run_publish`]
    /// for the full rationale — workspace and package share identical
    /// semantics here.
    async fn dry_run_publish(&self, config: &Config) -> Result<Option<PublishOutput>> {
        resolve_and_run_dry_run(
            self.path(),
            self.relative_path(),
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
    use rstest::rstest;
    use std::fs;
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
