use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::CSharpPackage, workspace::CSharpWorkspace};

#[derive(Debug)]
pub struct CSharpProjectFinder {
    projects: HashMap<PathBuf, Project>,
    project_files: Vec<&'static str>,
}

impl Default for CSharpProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl CSharpProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec![".csproj"],
        }
    }

    /// Extract the project name from the .csproj file path (filename without extension)
    fn extract_name_from_path(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }

    /// Extract version from .csproj XML content using quick-xml
    fn extract_version(content: &str) -> Option<String> {
        let mut reader = Reader::from_str(content);
        let mut buf = Vec::new();
        let mut in_property_group = false;
        let mut in_version = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = e.local_name();
                    if name.as_ref() == b"PropertyGroup" {
                        in_property_group = true;
                    } else if in_property_group && name.as_ref() == b"Version" {
                        in_version = true;
                    }
                }
                Ok(Event::End(e)) => {
                    let name = e.local_name();
                    if name.as_ref() == b"PropertyGroup" {
                        in_property_group = false;
                    } else if name.as_ref() == b"Version" {
                        in_version = false;
                    }
                }
                Ok(Event::Text(e)) => {
                    if in_version && let Ok(text) = e.unescape() {
                        let version = text.trim().to_string();
                        if !version.is_empty() {
                            return Some(version);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        None
    }

    /// Extract PackageReference dependencies from .csproj XML content using quick-xml
    #[allow(dead_code)]
    fn extract_package_references(content: &str) -> Vec<String> {
        let mut reader = Reader::from_str(content);
        let mut buf = Vec::new();
        let mut packages = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                    if e.local_name().as_ref() == b"PackageReference" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"Include"
                                && let Ok(value) = attr.unescape_value()
                            {
                                packages.push(value.to_string());
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        packages
    }

    /// Extract ProjectReference dependencies from .csproj XML content using quick-xml
    /// Returns the project names (extracted from paths)
    fn extract_project_references(content: &str) -> Vec<String> {
        let mut reader = Reader::from_str(content);
        let mut buf = Vec::new();
        let mut projects = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                    if e.local_name().as_ref() == b"ProjectReference" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"Include"
                                && let Ok(value) = attr.unescape_value()
                            {
                                // Extract project name from path like "..\CoreLib\CoreLib.csproj"
                                let path = Path::new(value.as_ref());
                                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                                    projects.push(name.to_string());
                                }
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        projects
    }

    /// Check if this project is part of a solution (workspace)
    /// A project is considered a workspace if there's a .sln file in the same directory
    fn is_workspace(path: &Path) -> bool {
        if let Some(parent) = path.parent() {
            // Check if there's a .sln file in the parent directory
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    if let Some(ext) = entry.path().extension()
                        && ext == "sln"
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[async_trait]
impl ProjectFinder for CSharpProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }

    fn projects_mut(&mut self) -> Vec<&mut Project> {
        self.projects.values_mut().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &self.project_files
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        // Check if this is a .csproj file
        if path.is_file() {
            let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

            if extension != "csproj" {
                return Ok(());
            }

            if self.projects.contains_key(path) {
                return Ok(());
            }

            // Read .csproj content
            let csproj_content = read_to_string(path).await?;

            let name = Self::extract_name_from_path(path);
            let version = Self::extract_version(&csproj_content);
            let is_workspace = Self::is_workspace(path);

            let (path_key, mut project) = if is_workspace {
                (
                    path.to_path_buf(),
                    Project::Workspace(Box::new(CSharpWorkspace::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                )
            } else {
                (
                    path.to_path_buf(),
                    Project::Package(Box::new(CSharpPackage::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                )
            };

            // Add ProjectReference dependencies (local project references)
            for dep in Self::extract_project_references(&csproj_content) {
                project.add_dependency(&dep);
            }

            self.projects.insert(path_key, project);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_new() {
        let finder = CSharpProjectFinder::new();
        assert_eq!(finder.project_files(), &[".csproj"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_default() {
        let finder = CSharpProjectFinder::default();
        assert_eq!(finder.project_files(), &[".csproj"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_visit_package() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
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

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj_path, &PathBuf::from("TestProject.csproj"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);
        match finder.projects()[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("TestProject"));
                assert_eq!(pkg.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_workspace_with_sln() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
        let sln_path = temp_dir.path().join("TestSolution.sln");

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

        fs::write(&sln_path, "Microsoft Visual Studio Solution File").unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj_path, &PathBuf::from("TestProject.csproj"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);
        match finder.projects()[0] {
            Project::Workspace(ws) => {
                assert_eq!(ws.name(), Some("TestProject"));
                assert_eq!(ws.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
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

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj_path, &PathBuf::from("TestProject.csproj"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);
        match finder.projects()[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("TestProject"));
                assert_eq!(pkg.version(), None);
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_non_csproj_file() {
        let temp_dir = TempDir::new().unwrap();
        let other_file = temp_dir.path().join("other.xml");
        fs::write(&other_file, r#"<root>content</root>"#).unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&other_file, &PathBuf::from("other.xml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_directory() {
        let temp_dir = TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("some_dir");
        fs::create_dir_all(&dir_path).unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&dir_path, &PathBuf::from("some_dir"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
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

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj_path, &PathBuf::from("TestProject.csproj"))
            .await
            .unwrap();
        finder
            .visit(&csproj_path, &PathBuf::from("TestProject.csproj"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_multiple_packages() {
        let temp_dir = TempDir::new().unwrap();
        let csproj1 = temp_dir.path().join("Project1").join("Project1.csproj");
        let csproj2 = temp_dir.path().join("Project2").join("Project2.csproj");
        fs::create_dir_all(csproj1.parent().unwrap()).unwrap();
        fs::create_dir_all(csproj2.parent().unwrap()).unwrap();
        fs::write(
            &csproj1,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();
        fs::write(
            &csproj2,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>2.0.0</Version>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj1, &PathBuf::from("Project1/Project1.csproj"))
            .await
            .unwrap();
        finder
            .visit(&csproj2, &PathBuf::from("Project2/Project2.csproj"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 2);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_projects_mut() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
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

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj_path, &PathBuf::from("TestProject.csproj"))
            .await
            .unwrap();

        let mut projects = finder.projects_mut();
        assert_eq!(projects.len(), 1);
        match &mut projects[0] {
            Project::Package(pkg) => {
                assert!(!pkg.is_changed());
                pkg.set_changed(true);
                assert!(pkg.is_changed());
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_with_project_references() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
        fs::write(
            &csproj_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <ProjectReference Include="..\CoreLib\CoreLib.csproj" />
    <ProjectReference Include="..\Utils\Utils.csproj" />
  </ItemGroup>
</Project>
"#,
        )
        .unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj_path, &PathBuf::from("TestProject.csproj"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("TestProject"));
                let deps = pkg.dependencies();
                // Only ProjectReferences are tracked (not PackageReferences)
                assert_eq!(deps.len(), 2);
                assert!(deps.contains("CoreLib"));
                assert!(deps.contains("Utils"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_extract_version() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.2.3</Version>
  </PropertyGroup>
</Project>"#;
        assert_eq!(
            CSharpProjectFinder::extract_version(content),
            Some("1.2.3".to_string())
        );

        let no_version = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
  </PropertyGroup>
</Project>"#;
        assert_eq!(CSharpProjectFinder::extract_version(no_version), None);
    }

    #[test]
    fn test_extract_package_references() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageReference Include="System.CommandLine" Version="2.0.0-beta4.22272.1" />
  </ItemGroup>
</Project>"#;
        let refs = CSharpProjectFinder::extract_package_references(content);
        assert_eq!(refs.len(), 2);
        assert!(refs.contains(&"Newtonsoft.Json".to_string()));
        assert!(refs.contains(&"System.CommandLine".to_string()));
    }

    #[test]
    fn test_extract_project_references() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <ProjectReference Include="..\CoreLib\CoreLib.csproj" />
    <ProjectReference Include="..\Utils\Utils.csproj" />
  </ItemGroup>
</Project>"#;
        let refs = CSharpProjectFinder::extract_project_references(content);
        assert_eq!(refs.len(), 2);
        assert!(refs.contains(&"CoreLib".to_string()));
        assert!(refs.contains(&"Utils".to_string()));
    }
}
