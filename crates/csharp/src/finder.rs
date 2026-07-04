use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder, is_regular_file};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::CSharpPackage, workspace::CSharpWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &[".csproj"];

#[derive(Debug, Default)]
pub struct CSharpProjectFinder {
    projects: HashMap<PathBuf, Project>,
}

impl CSharpProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
        }
    }

    /// Extract the project name from the .csproj file path (filename without extension)
    fn extract_name_from_path(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(std::string::ToString::to_string)
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
                    if in_version && let Ok(text) = e.decode() {
                        let version = text.trim().to_string();
                        if !version.is_empty() {
                            return Some(version);
                        }
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        None
    }

    /// Extract `ProjectReference` dependencies from .csproj XML content using quick-xml
    /// Returns the project names (extracted from paths)
    fn extract_project_references(content: &str) -> Vec<String> {
        let mut reader = Reader::from_str(content);
        let mut buf = Vec::new();
        let mut projects = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Empty(e) | Event::Start(e))
                    if e.local_name().as_ref() == b"ProjectReference" =>
                {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"Include"
                            && let Ok(value) = attr.normalized_value(XmlVersion::Implicit1_0)
                        {
                            // Extract project name from path like "..\CoreLib\CoreLib.csproj"
                            // Handle both Windows (\) and Unix (/) path separators
                            if let Some(name) = extract_project_name_from_path(&value) {
                                projects.push(name);
                            }
                        }
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        projects
    }

    /// Check if this project is part of a solution (workspace)
    /// A project is considered a workspace if there's a .sln file in the same directory
    async fn is_workspace(path: &Path) -> bool {
        if let Some(parent) = path.parent() {
            // Check if there's a .sln file in the parent directory
            if let Ok(mut entries) = tokio::fs::read_dir(parent).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
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

/// Extract project name from a path string, handling both Windows and Unix separators
/// Input: `"..\CoreLib\CoreLib.csproj"` or `"../CoreLib/CoreLib.csproj"`
/// Output: `"CoreLib"`
fn extract_project_name_from_path(path_str: &str) -> Option<String> {
    // Split by both Windows (\) and Unix (/) separators.
    // `rsplit(pat).next()` is documented to always return `Some(&str)`
    // (an input with no separator yields `Some(path_str)` intact, and
    // even `""` yields `Some("")`), so the previous `?` could never
    // short-circuit — it only misled readers into thinking a `None` arm
    // existed here. `strip_suffix(".csproj")` on the next line is the
    // sole actual `None` source for this function.
    let filename = path_str.rsplit(['\\', '/']).next().unwrap_or(path_str);

    // Remove .csproj extension
    filename
        .strip_suffix(".csproj")
        .map(std::string::ToString::to_string)
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
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        // Check if this is a .csproj file. Delegates to the shared
        // `is_regular_file` helper in `changepacks_core` (same
        // `unwrap_or(false)` fallthrough on stat errors as the previous
        // inline call — broken symlink / permission denied → "not a file").
        if is_regular_file(path).await {
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
            let is_workspace = Self::is_workspace(path).await;

            // Hoist the map key allocation out of both arms: the old shape
            // built a `(PathBuf, Project)` tuple, which forced each branch
            // to call `path.to_path_buf()` TWICE (once for the tuple slot,
            // once again for `*::new`). One shared `path_key` + one
            // `.clone()` into the constructor cuts 4 `PathBuf` allocs to 2.
            // Mirror of the same fix in `crates/java/src/finder.rs::visit`.
            let path_key = path.to_path_buf();
            let mut project = if is_workspace {
                Project::Workspace(Box::new(CSharpWorkspace::new(
                    name,
                    version,
                    path_key.clone(),
                    relative_path.to_path_buf(),
                )))
            } else {
                Project::Package(Box::new(CSharpPackage::new(
                    name,
                    version,
                    path_key.clone(),
                    relative_path.to_path_buf(),
                )))
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
    use rstest::rstest;
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

    // Fixtures for `test_extract_version` — one per XML shape the finder
    // must handle. Named consts keep each rstest `#[case]` line short and
    // self-describing.

    const XML_STANDARD_VERSION: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.2.3</Version>
  </PropertyGroup>
</Project>"#;

    const XML_NO_VERSION_ELEMENT: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
  </PropertyGroup>
</Project>"#;

    const XML_VERSION_WITH_END_TAG_WHITESPACE: &str = r#"<Project><PropertyGroup><Version>
   1.2.3
   </Version></PropertyGroup></Project>"#;

    const XML_EMPTY_VERSION: &str =
        r#"<Project><PropertyGroup><Version>  </Version></PropertyGroup></Project>"#;

    // Self-closing tags like <IsPackable /> generate Event::Empty, which
    // exercises the wildcard `_ => {}` arm in extract_version.
    const XML_VERSION_AFTER_EMPTY_ELEMENT: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <IsPackable />
    <Version>3.2.1</Version>
  </PropertyGroup>
</Project>"#;

    // XML comments generate Event::Comment, exercising the wildcard arm.
    const XML_VERSION_AFTER_COMMENT: &str = r#"<Project>
  <PropertyGroup>
    <!-- version follows -->
    <Version>4.0.0</Version>
  </PropertyGroup>
</Project>"#;

    #[rstest]
    // Standard `<Version>` inside `<PropertyGroup>`.
    #[case(XML_STANDARD_VERSION, Some("1.2.3"))]
    // No `<Version>` element at all → None.
    #[case(XML_NO_VERSION_ELEMENT, None)]
    // Whitespace/newlines around the version value are trimmed.
    #[case(XML_VERSION_WITH_END_TAG_WHITESPACE, Some("1.2.3"))]
    // Whitespace-only value returns None (empty after trim).
    #[case(XML_EMPTY_VERSION, None)]
    // Version element after a self-closing sibling (Event::Empty path).
    #[case(XML_VERSION_AFTER_EMPTY_ELEMENT, Some("3.2.1"))]
    // Version element after an XML comment (Event::Comment path).
    #[case(XML_VERSION_AFTER_COMMENT, Some("4.0.0"))]
    fn test_extract_version(#[case] content: &str, #[case] expected: Option<&str>) {
        assert_eq!(
            CSharpProjectFinder::extract_version(content),
            expected.map(std::string::ToString::to_string)
        );
    }

    #[test]
    fn test_extract_version_malformed_xml() {
        let content = "<Project><PropertyGroup><Version>1.0.0";
        // Should not panic - either returns Some or None
        let _ = CSharpProjectFinder::extract_version(content);
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

    #[rstest]
    // Windows-style paths (both single and doubled `..`).
    #[case(r"..\CoreLib\CoreLib.csproj", Some("CoreLib"))]
    #[case(r"..\..\Utils\Utils.csproj", Some("Utils"))]
    // Unix-style paths.
    #[case("../CoreLib/CoreLib.csproj", Some("CoreLib"))]
    // Just filename — no separator at all.
    #[case("MyProject.csproj", Some("MyProject"))]
    // Invalid — the extension is the sole legit `None` source.
    #[case("MyProject.txt", None)]
    fn test_extract_project_name_from_path(#[case] input: &str, #[case] expected: Option<&str>) {
        assert_eq!(
            super::extract_project_name_from_path(input),
            expected.map(std::string::ToString::to_string)
        );
    }
}
