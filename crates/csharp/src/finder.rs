use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder, has_extension_ignore_ascii_case, is_regular_file};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::CSharpPackage, xml_utils::is_unconditional_project_property_group};

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
        Self::default()
    }

    /// Extract the project name from the .csproj file path (filename without extension)
    fn extract_name_from_path(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(std::string::ToString::to_string)
    }

    /// Walk the .csproj XML ONCE and extract the project version, its
    /// `ProjectReference` dependency names, and default publishability in a
    /// single pass. The
    /// previous shape (`extract_version` + `extract_project_references`)
    /// ran two independent `quick_xml::Reader` passes over the identical
    /// bytes; merging them halves the parse cost on repos with many
    /// `.csproj` files (Unity / dotnet monorepos) while preserving existing
    /// version and project-reference behavior.
    fn parse_csproj_metadata(content: &str) -> Result<(Option<String>, Vec<String>, bool)> {
        let mut reader = Reader::from_str(content);
        // Preallocate the XML event buffer to skip the first few
        // geometric-doubling reallocations. Mirrors the
        // `Vec::with_capacity(paths.len())` preallocation policy already
        // applied across `sort_by_dep.rs`, `gen_update_map.rs`, and
        // `find_project_dirs.rs`. `read_event_into` calls `buf.clear()`
        // between events so the capacity persists; 256 bytes comfortably
        // covers the largest single event (attribute-laden `<Project Sdk=
        // "Microsoft.NET.Sdk"...>`, ~1-2 dozen bytes for the common
        // `<Version>` and `<ProjectReference>` shapes) without over-
        // reserving on tiny `.csproj` files.
        let mut buf = Vec::with_capacity(256);
        let mut eligible_property_group_depth = None;
        let mut in_version = false;
        let mut in_is_packable = false;
        let mut element_depth = 0usize;
        let mut project_depth = None;
        let mut version: Option<String> = None;
        let mut publishable_by_default = true;
        // Preallocate against the typical `<ProjectReference>` fan-out
        // observed in test fixtures (2 refs in
        // `test_visit_package_with_project_references`,
        // `test_extract_project_references`, and
        // `test_parse_csproj_metadata_returns_version_and_refs_in_one_pass`).
        // 4 comfortably covers the common 1-4 range without over-reserving
        // on `.csproj` files with zero project references. Closes the last
        // preallocation gap in this function and matches the
        // `Vec::with_capacity(256)` policy applied to `buf` right above —
        // the sibling preallocation policy shared with `sort_by_dep.rs`,
        // `gen_update_map.rs`, and `find_project_dirs.rs`.
        let mut projects: Vec<String> = Vec::with_capacity(4);

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    element_depth += 1;
                    let name = e.local_name();
                    if name.as_ref() == b"Project" && project_depth.is_none() {
                        project_depth = Some(element_depth);
                    } else if name.as_ref() == b"PropertyGroup"
                        && is_unconditional_project_property_group(
                            &e,
                            element_depth,
                            project_depth,
                        )?
                    {
                        eligible_property_group_depth = Some(element_depth);
                    } else if name.as_ref() == b"Version" {
                        in_version = eligible_property_group_depth
                            .is_some_and(|depth| element_depth == depth + 1);
                    } else if name.as_ref() == b"IsPackable" {
                        in_is_packable = eligible_property_group_depth
                            .is_some_and(|depth| element_depth == depth + 1);
                    } else if name.as_ref() == b"ProjectReference" {
                        collect_project_reference(&e, &mut projects)?;
                    }
                }
                Ok(Event::Empty(e)) if e.local_name().as_ref() == b"ProjectReference" => {
                    collect_project_reference(&e, &mut projects)?;
                }
                Ok(Event::End(e)) => {
                    let name = e.local_name();
                    if name.as_ref() == b"PropertyGroup"
                        && eligible_property_group_depth == Some(element_depth)
                    {
                        eligible_property_group_depth = None;
                    } else if name.as_ref() == b"Version" {
                        in_version = false;
                    } else if name.as_ref() == b"IsPackable" {
                        in_is_packable = false;
                    }
                    element_depth = element_depth
                        .checked_sub(1)
                        .context("unexpected XML end tag")?;
                }
                Ok(Event::Text(e)) => {
                    // Preserve the "first non-empty wins" semantics of the
                    // previous `extract_version` (which `return`ed early
                    // on the first hit) — later `<Version>` tags do NOT
                    // overwrite an earlier value.
                    if in_version
                        && version.is_none()
                        && let Ok(text) = e.decode()
                    {
                        let candidate = text.trim();
                        if !candidate.is_empty() {
                            version = Some(candidate.to_string());
                        }
                    }
                    if in_is_packable
                        && element_depth == 3
                        && let Ok(text) = e.decode()
                        && text.trim().eq_ignore_ascii_case("false")
                    {
                        publishable_by_default = false;
                    }
                }
                Ok(Event::CData(e)) => {
                    if in_version
                        && version.is_none()
                        && let Ok(text) = e.decode()
                    {
                        let candidate = text.trim();
                        if !candidate.is_empty() {
                            version = Some(candidate.to_string());
                        }
                    }
                    if in_is_packable
                        && element_depth == 3
                        && let Ok(text) = e.decode()
                        && text.trim().eq_ignore_ascii_case("false")
                    {
                        publishable_by_default = false;
                    }
                }
                Ok(Event::Eof) => {
                    anyhow::ensure!(element_depth == 0, "unexpected end of XML document");
                    break;
                }
                Err(error) => return Err(error.into()),
                _ => {}
            }
            buf.clear();
        }
        Ok((version, projects, publishable_by_default))
    }
}

/// Walk a `<ProjectReference Include="...">` / `Update="..."` element's attributes and
/// push its extracted project name into `projects`. Shared by both the
/// `Event::Start` and `Event::Empty` arms of `parse_csproj_metadata` so
/// the attribute-parsing lives in exactly one place.
fn collect_project_reference(
    e: &quick_xml::events::BytesStart<'_>,
    projects: &mut Vec<String>,
) -> Result<()> {
    let mut include_name = None;
    let mut update_name = None;

    for attr in e.attributes() {
        let attr = attr.context("Failed to parse ProjectReference attribute")?;
        let attr_name = attr.key.as_ref();
        if !matches!(attr_name, b"Include" | b"Update") {
            continue;
        }
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .context("Failed to normalize ProjectReference attribute value")?;
        let Some(name) = extract_project_name_from_path(&value) else {
            continue;
        };
        if attr_name == b"Include" {
            include_name = Some(name);
        } else {
            update_name = Some(name);
        }
    }

    if let Some(name) = include_name.or(update_name) {
        projects.push(name);
    }
    Ok(())
}

/// Extract project name from a path string, handling both Windows and Unix separators
/// Input: `"..\CoreLib\CoreLib.csproj"` or `"../CoreLib/CoreLib.csproj"`
/// Output: `"CoreLib"`
///
/// Case-insensitive `.csproj` match so `Include=".\Foo\Foo.CSPROJ"` (mixed-
/// case, common in older solutions and hand-written `.csproj` files) resolves
/// the same as the canonical `Foo.csproj`. The previous `strip_suffix(".csproj")`
/// was case-sensitive and silently dropped uppercase / mixed-case references,
/// which fed `sort_by_dependencies` a missing edge and skipped the reverse-dep
/// propagation in `apply_reverse_dependencies` on Windows-native repos.
/// Mirrors the case-insensitive extension gate now applied in `visit`.
fn extract_project_name_from_path(path_str: &str) -> Option<String> {
    // Split by both Windows (\) and Unix (/) separators; if there is no
    // separator, the whole `path_str` IS the filename. `rsplit_once` returns
    // `Some((prefix, tail))` when a separator is found and `None` otherwise,
    // so `map_or` falls back to `path_str` intact — self-documenting, no
    // unreachable panic surface. The extension gate below is the sole
    // actual `None` source for this function.
    let filename = path_str
        .rsplit_once(['\\', '/'])
        .map_or(path_str, |(_, tail)| tail);

    // Split filename on the LAST `.` so `Foo.csproj` → (`Foo`, `csproj`)
    // and `Foo.tests.csproj` → (`Foo.tests`, `csproj`). Then gate on the
    // extension using `eq_ignore_ascii_case` so mixed-case suffixes
    // (`.CSPROJ`, `.CsProj`) match the same as lowercase. Preserves the
    // previous `Option<String>` return and the "invalid extension → None"
    // contract byte-for-byte on the canonical `.csproj` case.
    let (stem, ext) = filename.rsplit_once('.')?;
    ext.eq_ignore_ascii_case("csproj").then(|| stem.to_string())
}

#[async_trait]
impl ProjectFinder for CSharpProjectFinder {
    changepacks_core::impl_projects_hashmap_accessors!();

    fn project_files(&self) -> &[&str] {
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        // Cheap-checks-first ordering (mirrors the `matches_project_file`
        // reorder in `changepacks-core`): reject on the file-extension
        // gate BEFORE hitting `tokio::fs::metadata`, so every non-
        // `.csproj` file in a `find_project_dirs` walk skips the async
        // stat entirely. On a 10 000-file monorepo with zero `.csproj`
        // entries this saves 10 000 stats per `visit` sweep.
        //
        // Extension match is case-insensitive so `.CSPROJ` /
        // `.CsProj` (mixed-case, common in Windows tooling and hand-
        // written project files) resolves the same as the canonical
        // lowercase form. Matches the case-insensitive suffix decoder
        // used by `extract_project_name_from_path`.
        if !has_extension_ignore_ascii_case(path, "csproj") {
            return Ok(());
        }

        if self.projects.contains_key(path) {
            return Ok(());
        }

        // Only after the cheap gates pass do we pay for a stat. Delegates
        // to the shared `is_regular_file` helper in `changepacks_core`
        // so missing paths and directories are skipped while other metadata
        // errors are propagated to the discovery caller.
        if !is_regular_file(path).await? {
            return Ok(());
        }

        // Read .csproj content
        let csproj_content = read_to_string(path)
            .await
            .with_context(|| format!("Failed to read C# project {}", path.display()))?;

        let name = Self::extract_name_from_path(path);
        // Single-pass metadata extraction — replaces the previous
        // `extract_version(...)` + `extract_project_references(...)`
        // pair that each constructed its own `quick_xml::Reader` and
        // walked the identical XML bytes. Halves parse work per
        // `.csproj` (meaningful on Unity/dotnet monorepos).
        let (version, project_refs, publishable_by_default) =
            Self::parse_csproj_metadata(&csproj_content)
                .with_context(|| format!("Failed to parse C# project XML: {}", path.display()))?;
        let path_key = path.to_path_buf();
        let relative_path_key = relative_path.to_path_buf();
        let mut project = Project::Package(Box::new(CSharpPackage::new_discovered(
            name,
            version,
            path_key.clone(),
            relative_path_key,
            publishable_by_default,
        )));

        // Add ProjectReference dependencies (local project references)
        // — `project_refs` came from the single-pass
        // `parse_csproj_metadata` call above, so no second walk of the
        // XML is needed here.
        for dep in project_refs {
            project.add_dependency(&dep);
        }

        self.projects.insert(path_key, project);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::UpdateType;
    use changepacks_utils::sort_by_dependencies;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;
    use tokio::fs as async_fs;

    struct VersionPolicyCase {
        name: &'static str,
        input: &'static str,
        discovered: Option<&'static str>,
        expected: &'static str,
    }

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
    async fn test_root_solution_csproj_manifests_are_packages() {
        let temp_dir = TempDir::new().unwrap();
        let library_path = temp_dir.path().join("Library.csproj");
        let app_path = temp_dir.path().join("App.csproj");
        fs::write(
            temp_dir.path().join("Product.sln"),
            "Microsoft Visual Studio Solution File",
        )
        .unwrap();
        fs::write(
            &library_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.2.3</Version>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();
        fs::write(
            &app_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>4.5.6</Version>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="Library.csproj" />
  </ItemGroup>
</Project>
"#,
        )
        .unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&app_path, Path::new("App.csproj"))
            .await
            .unwrap();
        finder
            .visit(&library_path, Path::new("Library.csproj"))
            .await
            .unwrap();

        let projects = sort_by_dependencies(finder.projects()).unwrap();
        assert_eq!(projects.len(), 2);
        assert!(
            projects
                .iter()
                .all(|project| matches!(project, Project::Package(_))),
            "solution-contained manifests must remain packages: {projects:?}"
        );
        assert_eq!(
            projects
                .iter()
                .map(|project| (project.name(), project.version(), project.relative_path()))
                .collect::<Vec<_>>(),
            vec![
                (Some("Library"), Some("1.2.3"), Path::new("Library.csproj"),),
                (Some("App"), Some("4.5.6"), Path::new("App.csproj")),
            ]
        );
        assert!(projects[1].dependencies().contains("Library"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_nested_solution_csproj_manifests_are_packages() {
        let temp_dir = TempDir::new().unwrap();
        let solution_dir = temp_dir.path().join("solutions").join("Product");
        let library_path = solution_dir
            .join("src")
            .join("Library")
            .join("Library.csproj");
        let app_path = solution_dir.join("src").join("App").join("App.csproj");
        fs::create_dir_all(library_path.parent().unwrap()).unwrap();
        fs::create_dir_all(app_path.parent().unwrap()).unwrap();
        fs::write(
            solution_dir.join("Product.sln"),
            "Microsoft Visual Studio Solution File",
        )
        .unwrap();
        fs::write(
            &library_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>2.0.0</Version>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();
        fs::write(
            &app_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>3.1.4</Version>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="..\Library\Library.csproj" />
  </ItemGroup>
</Project>
"#,
        )
        .unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&app_path, Path::new("solutions/Product/src/App/App.csproj"))
            .await
            .unwrap();
        finder
            .visit(
                &library_path,
                Path::new("solutions/Product/src/Library/Library.csproj"),
            )
            .await
            .unwrap();

        let projects = sort_by_dependencies(finder.projects()).unwrap();
        assert_eq!(projects.len(), 2);
        assert!(
            projects
                .iter()
                .all(|project| matches!(project, Project::Package(_))),
            "nested solution manifests must remain packages: {projects:?}"
        );
        assert_eq!(
            projects
                .iter()
                .map(|project| (project.name(), project.version(), project.relative_path()))
                .collect::<Vec<_>>(),
            vec![
                (
                    Some("Library"),
                    Some("2.0.0"),
                    Path::new("solutions/Product/src/Library/Library.csproj"),
                ),
                (
                    Some("App"),
                    Some("3.1.4"),
                    Path::new("solutions/Product/src/App/App.csproj"),
                ),
            ]
        );
        assert!(projects[1].dependencies().contains("Library"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_reads_version_from_cdata() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
        fs::write(
            &csproj_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version><![CDATA[1.2.3]]></Version>
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

        match finder.projects()[0] {
            Project::Package(pkg) => assert_eq!(pkg.version(), Some("1.2.3")),
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_ignores_sln_directory() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
        fs::create_dir(temp_dir.path().join("Fake.sln")).unwrap();
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

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(
            matches!(projects[0], Project::Package(_)),
            "expected Package when only a .sln directory exists, got {:?}",
            projects[0]
        );

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
            CSharpProjectFinder::parse_csproj_metadata(content)
                .unwrap()
                .0,
            expected.map(std::string::ToString::to_string)
        );
    }

    #[test]
    fn test_parse_csproj_metadata_is_packable_publishability() {
        let cases = [
            (
                "false",
                "<Project><PropertyGroup><IsPackable>false</IsPackable></PropertyGroup></Project>",
                false,
            ),
            (
                "trimmed mixed case false",
                "<Project><PropertyGroup><IsPackable>\n False\t </IsPackable></PropertyGroup></Project>",
                false,
            ),
            (
                "true",
                "<Project><PropertyGroup><IsPackable>true</IsPackable></PropertyGroup></Project>",
                true,
            ),
            (
                "missing",
                "<Project><PropertyGroup><Version>1.0.0</Version></PropertyGroup></Project>",
                true,
            ),
            (
                "self closing",
                "<Project><PropertyGroup><IsPackable /></PropertyGroup></Project>",
                true,
            ),
            (
                "conditional property group",
                r#"<Project><PropertyGroup Condition="'$(Configuration)' == 'Release'"><IsPackable>false</IsPackable></PropertyGroup></Project>"#,
                true,
            ),
            (
                "computed",
                "<Project><PropertyGroup><IsPackable>$(Packable)</IsPackable></PropertyGroup></Project>",
                true,
            ),
            (
                "nested property group",
                "<Project><Target><PropertyGroup><IsPackable>false</IsPackable></PropertyGroup></Target></Project>",
                true,
            ),
        ];

        for (label, content, expected) in cases {
            let publishable_by_default = CSharpProjectFinder::parse_csproj_metadata(content)
                .unwrap()
                .2;
            assert_eq!(publishable_by_default, expected, "{label}");
        }
    }

    #[tokio::test]
    async fn test_visit_package_carries_is_packable_false_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Private.csproj");
        fs::write(
            &csproj_path,
            "<Project><PropertyGroup><IsPackable>false</IsPackable></PropertyGroup></Project>",
        )
        .unwrap();

        let mut finder = CSharpProjectFinder::new();
        finder
            .visit(&csproj_path, Path::new("Private.csproj"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(!projects[0].is_publishable_by_default());
    }

    #[test]
    fn test_extract_version_malformed_xml() {
        let content = "<Project><PropertyGroup><Version>1.0.0";
        assert!(CSharpProjectFinder::parse_csproj_metadata(content).is_err());
    }

    #[tokio::test]
    async fn test_visit_malformed_xml_returns_path_context() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("Broken.csproj");
        fs::write(&csproj_path, "<Project><PropertyGroup><Version>1.0.0").unwrap();

        let mut finder = CSharpProjectFinder::new();
        let error = finder
            .visit(&csproj_path, &PathBuf::from("Broken.csproj"))
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Failed to parse C# project XML"));
        assert!(message.contains("Broken.csproj"));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_extract_project_references() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <ProjectReference Include="..\CoreLib\CoreLib.csproj" />
    <ProjectReference Include="..\Utils\Utils.csproj" />
    <ProjectReference Update="..\Updated\Updated.csproj" />
  </ItemGroup>
</Project>"#;
        let refs = CSharpProjectFinder::parse_csproj_metadata(content)
            .unwrap()
            .1;
        assert_eq!(refs.len(), 3);
        assert!(refs.contains(&"CoreLib".to_string()));
        assert!(refs.contains(&"Utils".to_string()));
        assert!(refs.contains(&"Updated".to_string()));
    }

    #[test]
    fn test_extract_project_references_prefers_include_over_update() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <ProjectReference Include="..\CoreLib\CoreLib.csproj" Update="..\Fallback\Fallback.csproj" />
  </ItemGroup>
</Project>"#;
        let refs = CSharpProjectFinder::parse_csproj_metadata(content)
            .unwrap()
            .1;
        assert_eq!(refs, vec!["CoreLib".to_string()]);
    }

    #[test]
    fn test_extract_project_references_from_start_and_empty_elements() {
        let content = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="..\Started\Started.csproj"></ProjectReference>
    <ProjectReference Include="..\Empty\Empty.csproj" />
  </ItemGroup>
</Project>"#;

        let refs = CSharpProjectFinder::parse_csproj_metadata(content)
            .unwrap()
            .1;

        assert_eq!(refs, vec!["Started".to_string(), "Empty".to_string()]);
    }

    #[test]
    fn test_project_reference_malformed_attribute_returns_contextual_error() {
        let content = r#"<Project><ItemGroup><ProjectReference Include="Valid.csproj" Broken /></ItemGroup></Project>"#;

        let error = CSharpProjectFinder::parse_csproj_metadata(content).unwrap_err();

        assert!(
            format!("{error:#}").contains("Failed to parse ProjectReference attribute"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn test_project_reference_malformed_entity_returns_contextual_error() {
        let content = r#"<Project><ItemGroup><ProjectReference Include="..\Bad&unknown;\Bad.csproj" /></ItemGroup></Project>"#;

        let error = CSharpProjectFinder::parse_csproj_metadata(content).unwrap_err();

        assert!(
            format!("{error:#}").contains("Failed to normalize ProjectReference attribute value"),
            "unexpected error: {error:#}"
        );
    }

    // The unified `parse_csproj_metadata` MUST return both the version and
    // the `ProjectReference` list in a single walk. This test fixes that
    // contract on a fixture combining both elements (plus a
    // `PackageReference` decoy that must be ignored) so any future refactor
    // that reintroduces a second XML walk — or accidentally drops one of
    // the outputs — trips a failing test immediately. Serves as the
    // regression anchor for the single-pass metadata-parse consolidation.
    #[test]
    fn test_parse_csproj_metadata_returns_version_and_refs_in_one_pass() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.5.0</Version>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <ProjectReference Include="..\CoreLib\CoreLib.csproj" />
    <ProjectReference Include="..\Utils\Utils.csproj" />
  </ItemGroup>
</Project>"#;
        let (version, refs, publishable_by_default) =
            CSharpProjectFinder::parse_csproj_metadata(content).unwrap();
        assert_eq!(version, Some("1.5.0".to_string()));
        assert_eq!(refs.len(), 2);
        assert!(refs.contains(&"CoreLib".to_string()));
        assert!(refs.contains(&"Utils".to_string()));
        assert!(publishable_by_default);
    }

    #[tokio::test]
    async fn test_discovery_and_rewrite_use_unconditional_top_level_property_groups() -> Result<()>
    {
        let cases = [
            VersionPolicyCase {
                name: "target-local",
                input: "<Project>\n  <Target Name=\"Build\">\n    <PropertyGroup>\n      <Version>7.0.0</Version>\n    </PropertyGroup>\n  </Target>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>",
                discovered: None,
                expected: "<Project>\n  <Target Name=\"Build\">\n    <PropertyGroup>\n      <Version>7.0.0</Version>\n    </PropertyGroup>\n  </Target>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <Version>0.0.1</Version>\n  </PropertyGroup>\n</Project>",
            },
            VersionPolicyCase {
                name: "conditional-only",
                input: "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version>7.0.0</Version>\n  </PropertyGroup>\n</Project>",
                discovered: None,
                expected: "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version>7.0.0</Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <Version>0.0.1</Version>\n  </PropertyGroup>\n</Project>",
            },
            VersionPolicyCase {
                name: "conditional-before-unconditional",
                input: "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version>7.0.0</Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <Version>1.2.3</Version>\n  </PropertyGroup>\n</Project>",
                discovered: Some("1.2.3"),
                expected: "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version>7.0.0</Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <Version>1.2.4</Version>\n  </PropertyGroup>\n</Project>",
            },
            VersionPolicyCase {
                name: "cdata",
                input: "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version><![CDATA[7.0.0]]></Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <Version><![CDATA[1.2.3]]></Version>\n  </PropertyGroup>\n</Project>",
                discovered: Some("1.2.3"),
                expected: "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version><![CDATA[7.0.0]]></Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <Version><![CDATA[1.2.4]]></Version>\n  </PropertyGroup>\n</Project>",
            },
            VersionPolicyCase {
                name: "self-closing",
                input: "<Project>\n  <Target Name=\"Build\">\n    <PropertyGroup>\n      <Version>7.0.0</Version>\n    </PropertyGroup>\n  </Target>\n  <PropertyGroup>\n    <Version/>\n  </PropertyGroup>\n</Project>",
                discovered: None,
                expected: "<Project>\n  <Target Name=\"Build\">\n    <PropertyGroup>\n      <Version>7.0.0</Version>\n    </PropertyGroup>\n  </Target>\n  <PropertyGroup>\n    <Version>0.0.1</Version>\n  </PropertyGroup>\n</Project>",
            },
            VersionPolicyCase {
                name: "namespaced",
                input: "<msb:Project xmlns:msb=\"urn:msbuild\">\n  <msb:PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <msb:Version>7.0.0</msb:Version>\n  </msb:PropertyGroup>\n  <msb:PropertyGroup>\n    <msb:Version>1.2.3</msb:Version>\n  </msb:PropertyGroup>\n</msb:Project>",
                discovered: Some("1.2.3"),
                expected: "<msb:Project xmlns:msb=\"urn:msbuild\">\n  <msb:PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <msb:Version>7.0.0</msb:Version>\n  </msb:PropertyGroup>\n  <msb:PropertyGroup>\n    <msb:Version>1.2.4</msb:Version>\n  </msb:PropertyGroup>\n</msb:Project>",
            },
            VersionPolicyCase {
                name: "crlf",
                input: "<Project>\r\n  <Target Name=\"Build\">\r\n    <PropertyGroup>\r\n      <Version>7.0.0</Version>\r\n    </PropertyGroup>\r\n  </Target>\r\n  <PropertyGroup>\r\n    <TargetFramework>net8.0</TargetFramework>\r\n  </PropertyGroup>\r\n</Project>\r\n",
                discovered: None,
                expected: "<Project>\r\n  <Target Name=\"Build\">\r\n    <PropertyGroup>\r\n      <Version>7.0.0</Version>\r\n    </PropertyGroup>\r\n  </Target>\r\n  <PropertyGroup>\r\n    <TargetFramework>net8.0</TargetFramework>\r\n    <Version>0.0.1</Version>\r\n  </PropertyGroup>\r\n</Project>\r\n",
            },
            VersionPolicyCase {
                name: "tab-indented",
                input: "<Project>\n\t<PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n\t\t<Version>7.0.0</Version>\n\t</PropertyGroup>\n\t<PropertyGroup>\n\t\t<Version>1.2.3</Version>\n\t</PropertyGroup>\n</Project>",
                discovered: Some("1.2.3"),
                expected: "<Project>\n\t<PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n\t\t<Version>7.0.0</Version>\n\t</PropertyGroup>\n\t<PropertyGroup>\n\t\t<Version>1.2.4</Version>\n\t</PropertyGroup>\n</Project>",
            },
        ];

        for case in cases {
            let temp_dir = TempDir::new()?;
            let manifest = temp_dir.path().join("Test.csproj");
            async_fs::write(&manifest, case.input).await?;
            let mut finder = CSharpProjectFinder::new();

            finder.visit(&manifest, Path::new("Test.csproj")).await?;
            {
                let mut projects = finder.projects_mut();
                let project = projects
                    .first_mut()
                    .context("finder did not return the C# fixture")?;
                assert_eq!(
                    project.version(),
                    case.discovered,
                    "{} discovery",
                    case.name
                );
                project.update_version(UpdateType::Patch).await?;
            }
            assert_eq!(
                async_fs::read_to_string(&manifest).await?,
                case.expected,
                "{} rewrite",
                case.name
            );
            temp_dir.close()?;
        }

        Ok(())
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
    // Case-insensitive `.csproj` — mixed-case suffixes (common in Windows
    // shell / hand-written project files) resolve the same as lowercase.
    // Regression anchor for the switch from `strip_suffix(".csproj")`
    // to `eq_ignore_ascii_case`.
    #[case("MyProject.CSPROJ", Some("MyProject"))]
    #[case("MyProject.CsProj", Some("MyProject"))]
    #[case(r"..\CoreLib\CoreLib.CSPROJ", Some("CoreLib"))]
    // No extension at all → None (rsplit_once('.') fails, function returns
    // early via `?`). Locks in the "no dot means no extension" contract.
    #[case("MyProject", None)]
    fn test_extract_project_name_from_path(#[case] input: &str, #[case] expected: Option<&str>) {
        assert_eq!(
            super::extract_project_name_from_path(input),
            expected.map(std::string::ToString::to_string)
        );
    }
}
