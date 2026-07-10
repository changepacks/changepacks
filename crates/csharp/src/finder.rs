use anyhow::{Context, Result};
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
    /// Memoize `is_workspace` results by each scanned ancestor directory.
    ///
    /// The workspace predicate is "does this directory or one of its
    /// ancestors contain a `.sln` file". Caching every scanned directory keeps
    /// flat solutions fast and prevents child-project layouts from re-scanning
    /// the same solution root for every `.csproj`.
    ///
    /// `HashMap<PathBuf, bool>` mirrors the shape already used for
    /// `self.projects` — no new dependency, no macro, no `Arc`.
    is_workspace_cache: HashMap<PathBuf, bool>,
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

    /// Walk the .csproj XML ONCE and extract both the project version and
    /// its `ProjectReference` dependency names in a single pass. The
    /// previous shape (`extract_version` + `extract_project_references`)
    /// ran two independent `quick_xml::Reader` passes over the identical
    /// bytes; merging them halves the parse cost on repos with many
    /// `.csproj` files (Unity / dotnet monorepos) with no behavior
    /// change.
    fn parse_csproj_metadata(content: &str) -> Result<(Option<String>, Vec<String>)> {
        let mut reader = Reader::from_str(content);
        // Preallocate the XML event buffer to skip the first few
        // geometric-doubling reallocations. Mirrors the
        // `Vec::with_capacity(paths.len())` preallocation policy already
        // applied across `sort_by_dep.rs`, `gen_update_map.rs`, and
        // `filter_project_dirs.rs`. `read_event_into` calls `buf.clear()`
        // between events so the capacity persists; 256 bytes comfortably
        // covers the largest single event (attribute-laden `<Project Sdk=
        // "Microsoft.NET.Sdk"...>`, ~1-2 dozen bytes for the common
        // `<Version>` and `<ProjectReference>` shapes) without over-
        // reserving on tiny `.csproj` files.
        let mut buf = Vec::with_capacity(256);
        let mut in_property_group = false;
        let mut in_version = false;
        let mut element_depth = 0usize;
        let mut version: Option<String> = None;
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
        // `gen_update_map.rs`, and `filter_project_dirs.rs`.
        let mut projects: Vec<String> = Vec::with_capacity(4);

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    element_depth += 1;
                    let name = e.local_name();
                    if name.as_ref() == b"PropertyGroup" {
                        in_property_group = true;
                    } else if in_property_group && name.as_ref() == b"Version" {
                        in_version = true;
                    } else if name.as_ref() == b"ProjectReference" {
                        collect_project_reference(&e, &mut projects);
                    }
                }
                Ok(Event::Empty(e)) if e.local_name().as_ref() == b"ProjectReference" => {
                    collect_project_reference(&e, &mut projects);
                }
                Ok(Event::End(e)) => {
                    element_depth = element_depth
                        .checked_sub(1)
                        .context("unexpected XML end tag")?;
                    let name = e.local_name();
                    if name.as_ref() == b"PropertyGroup" {
                        in_property_group = false;
                    } else if name.as_ref() == b"Version" {
                        in_version = false;
                    }
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
        Ok((version, projects))
    }

    /// Check if this project is part of a solution (workspace)
    /// A project is considered a workspace if it has a `.sln` file in its
    /// directory or any ancestor directory.
    ///
    /// Flattened from the previous three-level `if let Some(parent) → if
    /// let Ok(entries) → while let Ok(Some(entry))` pyramid to two
    /// `let ... else { return false; }` bindings + the `while let`
    /// loop. Same predicate, same fallthrough on any error, same
    /// short-circuit on the first `.sln` hit — byte-identical behavior,
    /// one indentation level. Idiomatic modern Rust (edition 2024) and
    /// matches the same let-else style already used in this crate.
    ///
    /// Memoized by scanned directory (not the `.csproj` path itself):
    /// the predicate answers whether that directory holds a `.sln` marker.
    /// A child project checks its own directory first, then walks upward until
    /// a solution root is found or all ancestors have been scanned.
    async fn is_workspace(&mut self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        for dir in parent.ancestors() {
            if let Some(&cached) = self.is_workspace_cache.get(dir) {
                if cached {
                    return true;
                }
                continue;
            }

            let Some(has_solution) = dir_has_solution_file(dir).await else {
                continue;
            };
            self.is_workspace_cache
                .insert(dir.to_path_buf(), has_solution);
            if has_solution {
                return true;
            }
        }
        false
    }
}

async fn dir_has_solution_file(dir: &Path) -> Option<bool> {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        // Do NOT cache read_dir failures: a later visit may succeed
        // (e.g. transient permission race), and a failed scan should not
        // poison ancestor detection for the rest of the finder lifetime.
        return None;
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                // Case-insensitive `.sln` match so a Windows-native `Solution.SLN` /
                // `Foo.Sln` is recognized the same as lowercase `.sln`.
                let file_name = entry.file_name();
                let has_sln_extension = Path::new(&file_name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("sln"));
                if has_sln_extension {
                    let Ok(file_type) = entry.file_type().await else {
                        return None;
                    };
                    if !file_type.is_file() {
                        continue;
                    }
                    return Some(true);
                }
            }
            Ok(None) => return Some(false),
            Err(_) => return None,
        }
    }
}

/// Walk a `<ProjectReference Include="...">` / `Update="..."` element's attributes and
/// push its extracted project name into `projects`. Shared by both the
/// `Event::Start` and `Event::Empty` arms of `parse_csproj_metadata` so
/// the attribute-parsing lives in exactly one place.
fn collect_project_reference(e: &quick_xml::events::BytesStart<'_>, projects: &mut Vec<String>) {
    let mut include_name = None;
    let mut update_name = None;

    for attr in e.attributes().flatten() {
        let attr_name = attr.key.as_ref();
        if !matches!(attr_name, b"Include" | b"Update") {
            continue;
        }
        let Ok(value) = attr.normalized_value(XmlVersion::Implicit1_0) else {
            continue;
        };
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
        let matches_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("csproj"));
        if !matches_ext {
            return Ok(());
        }

        if self.projects.contains_key(path) {
            return Ok(());
        }

        // Only after the cheap gates pass do we pay for a stat. Delegates
        // to the shared `is_regular_file` helper in `changepacks_core`
        // (same `unwrap_or(false)` fallthrough on stat errors as the
        // previous inline call — broken symlink / permission denied →
        // "not a file").
        if !is_regular_file(path).await {
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
        let (version, project_refs) = Self::parse_csproj_metadata(&csproj_content)
            .with_context(|| format!("Failed to parse C# project XML: {}", path.display()))?;
        let is_workspace = self.is_workspace(path).await;

        // Hoist the map key allocation out of both arms: the old shape
        // built a `(PathBuf, Project)` tuple, which forced each branch
        // to call `path.to_path_buf()` TWICE (once for the tuple slot,
        // once again for `*::new`). One shared `path_key` + one
        // `.clone()` into the constructor cuts 4 `PathBuf` allocs to 2.
        // Mirror of the same fix in `crates/java/src/finder.rs::visit`.
        let path_key = path.to_path_buf();
        let relative_path_key = relative_path.to_path_buf();
        let mut project = if is_workspace {
            Project::Workspace(Box::new(CSharpWorkspace::new(
                name,
                version,
                path_key.clone(),
                relative_path_key,
            )))
        } else {
            Project::Package(Box::new(CSharpPackage::new(
                name,
                version,
                path_key.clone(),
                relative_path_key,
            )))
        };

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

    /// Regression: a Windows-native uppercase `.SLN` sibling must classify
    /// the `.csproj` as `Project::Workspace`, exactly like a lowercase
    /// `.sln`. Locks in the case-insensitive `is_workspace` extension gate
    /// so a future revert to a case-sensitive `ext == "sln"` compare (which
    /// silently misclassified `MySolution.SLN` projects as `Package`) trips
    /// immediately. Mirrors the case-insensitive `.csproj` coverage already
    /// asserted in `test_extract_project_name_from_path`.
    #[tokio::test]
    async fn test_visit_workspace_with_uppercase_sln() {
        let temp_dir = TempDir::new().unwrap();
        let csproj_path = temp_dir.path().join("TestProject.csproj");
        let sln_path = temp_dir.path().join("TestSolution.SLN");

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
            _ => panic!("Expected Workspace (uppercase .SLN must be recognized)"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_dir_has_solution_file_returns_true_for_sln_file() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("Solution.sln"),
            "Microsoft Visual Studio Solution File",
        )
        .unwrap();

        assert_eq!(dir_has_solution_file(temp_dir.path()).await, Some(true));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_dir_has_solution_file_returns_false_without_sln() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("README.md"), "not a solution").unwrap();

        assert_eq!(dir_has_solution_file(temp_dir.path()).await, Some(false));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_dir_has_solution_file_ignores_sln_directory() {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir(temp_dir.path().join("Fake.sln")).unwrap();

        assert_eq!(dir_has_solution_file(temp_dir.path()).await, Some(false));

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

    /// Regression: locks in the "cache hits on the second call" contract
    /// for `is_workspace`. Two sibling `.csproj` files share a directory
    /// that also holds a `.sln`; visiting both via ONE `CSharpProjectFinder`
    /// must produce a cache with exactly ONE entry — proving the second
    /// visit reused the first's `read_dir` result instead of re-scanning
    /// the sibling directory. A future refactor that silently drops the
    /// cache would fail this test immediately.
    ///
    /// Complements `test_visit_workspace_with_sln`: that test asserts the
    /// classification is correct; this test asserts the SYSCALL SAVINGS
    /// is real. Together they pin both halves of the retry-now#0029
    /// improvement (correct classification AND deduplicated read_dir).
    #[tokio::test]
    async fn test_is_workspace_cache_reuses_result_for_siblings() {
        let temp_dir = TempDir::new().unwrap();
        let sln_path = temp_dir.path().join("Solution.sln");
        let csproj1 = temp_dir.path().join("Project1.csproj");
        let csproj2 = temp_dir.path().join("Project2.csproj");

        std::fs::write(&sln_path, "Microsoft Visual Studio Solution File").unwrap();
        std::fs::write(
            &csproj1,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>
"#,
        )
        .unwrap();
        std::fs::write(
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
            .visit(&csproj1, &PathBuf::from("Project1.csproj"))
            .await
            .unwrap();
        finder
            .visit(&csproj2, &PathBuf::from("Project2.csproj"))
            .await
            .unwrap();

        // Both projects classified as Workspace via the .sln sibling.
        assert_eq!(finder.projects().len(), 2);
        for project in finder.projects() {
            assert!(
                matches!(project, Project::Workspace(_)),
                "expected Workspace, got {project:?}"
            );
        }
        // Exactly ONE cache entry: the shared parent directory. Both
        // sibling visits resolved to the same key, so the second call
        // hit the cache and skipped read_dir.
        assert_eq!(
            finder.is_workspace_cache.len(),
            1,
            "expected exactly one cache entry (shared parent), got {:?}",
            finder.is_workspace_cache
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_workspace_with_ancestor_sln() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("Project1");
        let csproj_path = project_dir.join("Project1.csproj");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            temp_dir.path().join("Solution.sln"),
            "Microsoft Visual Studio Solution File",
        )
        .unwrap();
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
            .visit(&csproj_path, &PathBuf::from("Project1/Project1.csproj"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(
            matches!(projects[0], Project::Workspace(_)),
            "expected ancestor .sln to classify child project as Workspace, got {:?}",
            projects[0]
        );
        assert_eq!(finder.is_workspace_cache.get(&project_dir), Some(&false));
        assert_eq!(finder.is_workspace_cache.get(temp_dir.path()), Some(&true));

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

    // The unified `parse_csproj_metadata` MUST return both the version and
    // the `ProjectReference` list in a single walk. This test fixes that
    // contract on a fixture combining both elements (plus a
    // `PackageReference` decoy that must be ignored) so any future refactor
    // that reintroduces a second XML walk — or accidentally drops one of
    // the outputs — trips a failing test immediately. Serves as the
    // regression anchor called out in the batch plan for iteration 0027.
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
        let (version, refs) = CSharpProjectFinder::parse_csproj_metadata(content).unwrap();
        assert_eq!(version, Some("1.5.0".to_string()));
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
    // Case-insensitive `.csproj` — mixed-case suffixes (common in Windows
    // shell / hand-written project files) resolve the same as lowercase.
    // Regression anchor for the batch-plan item that flipped
    // `strip_suffix(".csproj")` to `eq_ignore_ascii_case`.
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
