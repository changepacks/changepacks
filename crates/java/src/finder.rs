use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
#[cfg(test)]
use std::process::Stdio;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;
use tokio::process::Command;

use crate::{
    gradle_dependency_lexer::{extract_gradle_project_dependencies, gradle_dependency_dialect},
    gradle_metadata::{GradleProperties, GradleWrapperMetadata, get_gradle_metadata},
    package::GradlePackage,
    workspace::GradleWorkspace,
};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["build.gradle.kts", "build.gradle"];

/// OS-specific Java executable filename, used by `which_java_in` and
/// `java_home_has_java` to avoid repeating the `cfg!(windows)` branch.
#[cfg(windows)]
const JAVA_EXECUTABLE: &str = "java.exe";
#[cfg(not(windows))]
const JAVA_EXECUTABLE: &str = "java";

#[derive(Debug, Default)]
pub struct GradleProjectFinder {
    projects: HashMap<PathBuf, Project>,
    java_available: Option<bool>,
    metadata_by_wrapper: HashMap<PathBuf, GradleWrapperMetadata>,
    /// Raw `gradlew_dir` (as returned by `find_gradlew`) to its canonicalized
    /// form. Every subproject of a Gradle monorepo resolves to the SAME wrapper
    /// root, so without this cache each visited manifest repeats an identical
    /// `canonicalize` syscall just to build the `metadata_by_wrapper` key.
    wrapper_dir_canonical: HashMap<PathBuf, PathBuf>,
}

impl GradleProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the repository-bounded Gradle wrapper for `manifest_path` and make
    /// sure this finder holds that wrapper's batched metadata.
    ///
    /// Owns the three cache-shaped steps of discovery: the bounded `find_gradlew`
    /// ancestor walk, the `wrapper_dir_canonical` memoization of the wrapper root,
    /// and the one-shot `metadata_by_wrapper` fill. Returns the normalized wrapper
    /// root (the `metadata_by_wrapper` key) together with the wrapper path, which
    /// the caller still needs for its error context.
    async fn resolve_wrapper_metadata(
        &mut self,
        manifest_path: &Path,
        project_dir: &Path,
        relative_path: &Path,
        java_available: bool,
    ) -> Result<(PathBuf, PathBuf)> {
        // Bound the gradlew search to the repository root: `relative_path` is
        // the build file's path relative to the git repo root, so its component
        // count equals the number of directories from `project_dir` up to and
        // INCLUDING the repo root (root project: `build.gradle.kts` → count 1 →
        // check `project_dir` only). This stops the ancestor walk at the repo
        // boundary so an out-of-repo `gradlew` is never discovered or executed.
        // Mirrors the C# finder's `is_workspace` bound.
        let max_depth = relative_path.components().count();

        let (gradlew, gradlew_dir) = find_gradlew(project_dir, max_depth)
            .await?
            .with_context(|| gradlew_not_found(manifest_path))?;
        // Sibling subprojects all report the same `gradlew_dir`, so canonicalize
        // it once per wrapper root instead of once per manifest.
        let normalized_wrapper_dir = match self.wrapper_dir_canonical.get(&gradlew_dir) {
            Some(cached) => cached.clone(),
            None => {
                let normalized =
                    tokio::fs::canonicalize(&gradlew_dir)
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to normalize Gradle wrapper root '{}' for '{}'",
                                gradlew_dir.display(),
                                manifest_path.display()
                            )
                        })?;
                self.wrapper_dir_canonical
                    .insert(gradlew_dir.clone(), normalized.clone());
                normalized
            }
        };

        if !self
            .metadata_by_wrapper
            .contains_key(&normalized_wrapper_dir)
        {
            let metadata = get_gradle_metadata(&gradlew, &gradlew_dir, java_available).await?;
            self.metadata_by_wrapper
                .insert(normalized_wrapper_dir.clone(), metadata);
        }

        Ok((normalized_wrapper_dir, gradlew))
    }

    /// Look up the metadata record Gradle emitted for one project directory.
    ///
    /// Both lookups (wrapper record, then project record inside it) report the
    /// same context, so the shared closure lives here instead of inline in
    /// `visit`. The wrapper record is handed back alongside the project's Gradle
    /// path and properties because the caller resolves dependency project names
    /// against that same record and must not repeat the lookup.
    fn gradle_project_metadata<'a>(
        &'a self,
        normalized_wrapper_dir: &Path,
        project_dir: &Path,
        normalized_project_dir: &Path,
        gradlew: &Path,
    ) -> Result<(&'a GradleWrapperMetadata, String, GradleProperties)> {
        let missing_metadata_context = || {
            format!(
                "missing Gradle metadata record for project directory '{}' (normalized: '{}') from wrapper '{}'",
                project_dir.display(),
                normalized_project_dir.display(),
                gradlew.display()
            )
        };
        let wrapper_metadata = self
            .metadata_by_wrapper
            .get(normalized_wrapper_dir)
            .with_context(missing_metadata_context)?;
        let metadata = wrapper_metadata
            .by_project_dir
            .get(normalized_project_dir)
            .with_context(missing_metadata_context)?;
        Ok((
            wrapper_metadata,
            metadata.project_path.clone(),
            metadata.properties.clone(),
        ))
    }

    /// Map the `project(":a:b")` dependency paths lexed out of a build file to
    /// the project names Gradle reported for them.
    ///
    /// A miss means the build file references a project the wrapper never
    /// emitted, so the error names every field needed to locate the mismatch.
    fn dependency_project_names<'a>(
        project_names_by_path: &'a HashMap<String, String>,
        dependencies: &[&str],
        name: Option<&str>,
        project_path: &str,
        manifest_path: &Path,
        gradlew: &Path,
    ) -> Result<Vec<&'a String>> {
        dependencies
            .iter()
            .map(|dependency_path| {
                project_names_by_path
                    .get(*dependency_path)
                    .with_context(|| {
                        format!(
                            "Gradle dependency project path '{}' declared by project '{}' (Gradle path '{}', manifest '{}') is missing from metadata emitted by wrapper '{}'",
                            dependency_path,
                            name.unwrap_or("<unnamed>"),
                            project_path,
                            manifest_path.display(),
                            gradlew.display()
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()
    }
}

async fn is_java_executable_candidate(path: &Path) -> Result<bool> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to read metadata for {}", path.display()));
        }
    };
    if !metadata.is_file() {
        return Ok(false);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        Ok(true)
    }
}

/// Core logic for finding `java` in a given PATH value.
///
/// Scans the split paths for a `java` / `java.exe` executable.
/// Returns `None` if `path_var` is `None` or empty.
///
/// Metadata errors other than missing candidates are propagated.
///
/// This function is testable without mutating process env.
async fn which_java_in(path_var: Option<&OsStr>) -> Result<Option<PathBuf>> {
    let Some(path_var) = path_var else {
        return Ok(None);
    };
    if path_var.is_empty() {
        return Ok(None);
    }
    for dir in std::env::split_paths(path_var) {
        let candidate = dir.join(JAVA_EXECUTABLE);
        if is_java_executable_candidate(&candidate).await? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

async fn java_home_has_java(java_home: Option<&OsStr>) -> Result<bool> {
    let Some(java_home) = java_home else {
        return Ok(false);
    };
    if java_home.is_empty() {
        return Ok(false);
    }

    let candidate = Path::new(java_home).join("bin").join(JAVA_EXECUTABLE);
    is_java_executable_candidate(&candidate).await
}

/// The platform-appropriate Gradle wrapper filename.
fn gradle_wrapper_name(windows: bool) -> &'static str {
    if windows { "gradlew.bat" } else { "gradlew" }
}

/// Single source of truth for the "wrapper missing" error message.
///
/// Both the discovery path (`GradleProjectFinder::visit`) and the publish path
/// (`run_gradle_publish`) fail here, and each names the offending manifest the
/// same way every other fallible step in those functions does. The leading
/// sentence is a stability contract: `crates/java/src/lib.rs` and
/// `crates/java/src/package.rs` assert on
/// `.contains("Gradle wrapper (gradlew) not found")`, so the prefix must stay
/// byte-identical and the manifest path is appended after it.
fn gradlew_not_found(manifest: &Path) -> String {
    format!(
        "Gradle wrapper (gradlew) not found for '{}'. \
         Ensure the project root contains gradlew or gradlew.bat.",
        manifest.display()
    )
}

async fn find_gradlew(start_dir: &Path, max_depth: usize) -> Result<Option<(PathBuf, PathBuf)>> {
    find_gradlew_named(start_dir, max_depth, gradle_wrapper_name(cfg!(windows))).await
}

/// Find gradlew executable by walking up the directory tree.
///
/// In multi-module Gradle builds, `gradlew` lives at the root while subprojects
/// only contain `build.gradle.kts`. This function searches upward from `start_dir`
/// until it finds `gradlew` (Unix) or `gradlew.bat` (Windows).
///
/// The ancestor walk is BOUNDED to the repository root by `max_depth`: the
/// caller passes `relative_path.components().count()` — the number of
/// directories from the project dir up to and INCLUDING the repo root — so
/// `start_dir.ancestors().take(max_depth)` stops AT the repository root and
/// never touches the drive root, the user's home dir, or a sibling checkout.
/// An out-of-repo `gradlew` must never be discovered (and then executed):
/// project discovery is git-scoped, so a stray wrapper ABOVE the repo root
/// must not be picked up and run. Mirrors the git-scoped bounds the sibling
/// C# finder applies in `is_workspace` and the Rust finder applies in its
/// version-inheritance walk.
///
/// Returns `(gradlew_path, gradlew_dir)`, or `None` if not found within the bound.
async fn find_gradlew_named(
    start_dir: &Path,
    max_depth: usize,
    gradlew_name: &str,
) -> Result<Option<(PathBuf, PathBuf)>> {
    // `Path::ancestors()` yields `[start_dir, parent, …, root]`; `take(max_depth)`
    // caps the climb at the repository root so the walk never leaves the repo
    // and can never adopt an out-of-repo wrapper.
    for current in start_dir.ancestors().take(max_depth) {
        let gradlew = current.join(gradlew_name);
        // Reject directories while continuing the bounded search; propagate
        // metadata failures other than a missing wrapper candidate.
        if changepacks_core::is_regular_file(&gradlew).await? {
            return Ok(Some((gradlew, current.to_path_buf())));
        }
    }
    Ok(None)
}

/// Run a built-in Gradle publish task through the repository-bounded wrapper.
///
/// The wrapper and task are passed as OS arguments rather than interpolated
/// into a shell command, so paths containing spaces or shell metacharacters
/// remain intact. Configured publish commands do not use this path; their
/// existing shell semantics are preserved by the package/workspace callers.
pub(crate) async fn run_gradle_publish(
    manifest_path: &Path,
    relative_path: &Path,
    project_path: Option<&str>,
    task: &str,
    additional_args: &[OsString],
    missing_dir_ctx: &'static str,
) -> Result<changepacks_core::publish::PublishOutput> {
    let project_dir = manifest_path.parent().context(missing_dir_ctx)?;
    let max_depth = relative_path.components().count();
    let (gradlew, gradlew_dir) = find_gradlew(project_dir, max_depth)
        .await?
        .with_context(|| gradlew_not_found(manifest_path))?;
    let mut args = Vec::with_capacity(additional_args.len() + 1);
    args.push(match project_path {
        Some(project_path) => gradle_task_arg_from_project_path(project_path, task),
        None => gradle_task_arg_from_project_dir(project_dir, &gradlew_dir, task)?,
    });
    args.extend_from_slice(additional_args);
    let output = GradleCommandSpec::new(&gradlew, &gradlew_dir, args)
        .command()
        .output()
        .await
        .with_context(|| format!("Failed to execute Gradle wrapper '{}'", gradlew.display()))?;

    Ok(output.into())
}

fn gradle_subproject_path(relative: &Path) -> Result<String> {
    // Preallocate against the source path's byte length: each `:` separator we
    // push is 1 byte and maps 1:1 to a path-separator byte already counted in
    // `as_os_str().len()`, so that length is a safe upper bound for the joined
    // `:`-separated output — removing the geometric-doubling reallocations for
    // deep subprojects. Matches the preallocation policy used elsewhere in the
    // finders.
    let mut path = String::with_capacity(relative.as_os_str().len());
    for component in relative.components() {
        let value = component.as_os_str().to_str().with_context(|| {
            format!(
                "Gradle subproject path contains a non-Unicode component: {}",
                relative.display()
            )
        })?;
        if !path.is_empty() {
            path.push(':');
        }
        path.push_str(value);
    }
    Ok(path)
}

/// Returns true when a Java runtime is available via JAVA_HOME or PATH.
async fn java_is_available() -> Result<bool> {
    let java_home = std::env::var_os("JAVA_HOME");
    if java_home_has_java(java_home.as_deref()).await? {
        return Ok(true);
    }
    let path = std::env::var_os("PATH");
    Ok(which_java_in(path.as_deref()).await?.is_some())
}

fn gradle_task_arg_from_project_path(project_path: &str, task: &str) -> OsString {
    if project_path == ":" {
        OsString::from(task)
    } else {
        OsString::from(format!("{project_path}:{task}"))
    }
}

fn gradle_task_arg_from_project_dir(
    project_dir: &Path,
    gradlew_dir: &Path,
    task: &str,
) -> Result<OsString> {
    if gradlew_dir == project_dir {
        return Ok(OsString::from(task));
    }

    let relative = project_dir
        .strip_prefix(gradlew_dir)
        .context("Failed to compute subproject path")?;
    let gradle_path = gradle_subproject_path(relative)?;
    Ok(OsString::from(format!(":{gradle_path}:{task}")))
}

/// Argument/working-directory bundle for one Gradle wrapper invocation.
///
/// Shared by `finder.rs` (publish tasks) and `gradle_metadata.rs` (batched
/// metadata discovery), so it stays here as `pub(crate)`.
#[derive(Debug)]
pub(crate) struct GradleCommandSpec {
    program: OsString,
    args: Vec<OsString>,
    current_dir: PathBuf,
}

impl GradleCommandSpec {
    pub(crate) fn new(gradlew: &Path, gradlew_dir: &Path, gradle_args: Vec<OsString>) -> Self {
        let mut args = Vec::with_capacity(gradle_args.len() + usize::from(!cfg!(windows)));
        let program = if cfg!(windows) {
            gradlew.as_os_str().to_owned()
        } else {
            args.push(gradlew.as_os_str().to_owned());
            OsString::from("sh")
        };
        args.extend(gradle_args);

        Self {
            program,
            args,
            current_dir: gradlew_dir.to_path_buf(),
        }
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.current_dir)
            .kill_on_drop(true);
        command
    }
}

#[async_trait]
impl ProjectFinder for GradleProjectFinder {
    changepacks_core::impl_projects_hashmap_accessors!();

    fn project_files(&self) -> &[&str] {
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        // Parse this manifest if it is a recognized project file not already
        // visited. Both guards live in `ProjectFinder::should_visit_manifest`
        // (name/stat gate first, already-discovered map probe second) so the
        // prelude is written once for every file-name-based finder.
        if !self.should_visit_manifest(path).await? {
            return Ok(());
        }

        let project_dir = path
            .parent()
            .with_context(|| format!("Parent not found - {}", path.display()))?;

        let java_available = match self.java_available {
            Some(value) => value,
            None => {
                let value = java_is_available().await?;
                self.java_available = Some(value);
                value
            }
        };

        // Read Gradle build file first (fail fast if unreadable)
        let content = read_to_string(path)
            .await
            .with_context(|| format!("Failed to read Gradle build file {}", path.display()))?;
        let dependencies =
            extract_gradle_project_dependencies(&content, gradle_dependency_dialect(path));

        let (normalized_wrapper_dir, gradlew) = self
            .resolve_wrapper_metadata(path, project_dir, relative_path, java_available)
            .await?;

        let normalized_project_dir =
            tokio::fs::canonicalize(project_dir)
                .await
                .with_context(|| {
                    format!(
                        "Failed to normalize Gradle project directory '{}' for '{}'",
                        project_dir.display(),
                        path.display()
                    )
                })?;
        let (wrapper_metadata, project_path, properties) = self.gradle_project_metadata(
            &normalized_wrapper_dir,
            project_dir,
            &normalized_project_dir,
            &gradlew,
        )?;
        let GradleProperties {
            name,
            version,
            has_subprojects,
            has_publish_task,
            has_publish_to_maven_local_task,
        } = properties;

        // Use directory name as fallback for project name
        let name = name.or_else(|| {
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(std::string::ToString::to_string)
        });

        let dependency_names = Self::dependency_project_names(
            &wrapper_metadata.project_names_by_path,
            &dependencies,
            name.as_deref(),
            &project_path,
            path,
            &gradlew,
        )?;

        // Workspace detection: gradlew reports non-empty subprojects list.
        // Previous approach (checking for settings.gradle.kts existence) caused
        // false positives in composite builds and subprojects with IDE-generated files.
        let is_workspace = has_subprojects;

        // Hoist the map key allocation out of both arms: the old shape
        // built a `(PathBuf, Project)` tuple, which forced each branch
        // to call `path.to_path_buf()` TWICE (once for the tuple slot,
        // once again for `*::new`). One shared `path_key` + one
        // `.clone()` into the constructor cuts 4 `PathBuf` allocs to 2.
        let path_key = path.to_path_buf();
        let relative_path_key = relative_path.to_path_buf();
        let mut project = if is_workspace {
            Project::Workspace(Box::new(
                GradleWorkspace::new_with_project_path_and_publish_tasks(
                    name,
                    version,
                    path_key.clone(),
                    relative_path_key,
                    Some(project_path),
                    has_publish_task,
                    has_publish_to_maven_local_task,
                ),
            ))
        } else {
            Project::Package(Box::new(
                GradlePackage::new_with_project_path_and_publish_tasks(
                    name,
                    version,
                    path_key.clone(),
                    relative_path_key,
                    Some(project_path),
                    has_publish_task,
                    has_publish_to_maven_local_task,
                ),
            ))
        };

        for dependency in dependency_names {
            project.add_dependency(dependency);
        }

        self.projects.insert(path_key, project);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gradle_metadata::GRADLE_METADATA_PREFIX;
    use changepacks_core::{Project, UpdateType};
    use changepacks_utils::{apply_reverse_dependencies, sort_by_dependencies};
    use rstest::rstest;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    fn finder_with_java_available() -> GradleProjectFinder {
        GradleProjectFinder {
            java_available: Some(true),
            ..GradleProjectFinder::default()
        }
    }

    async fn dependencies_for_manifest(manifest_name: &str, content: &str) -> HashSet<String> {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("project");
        tokio::fs::create_dir_all(&project_dir).await.unwrap();
        let manifest = project_dir.join(manifest_name);
        tokio::fs::write(&manifest, content).await.unwrap();
        let dependency_paths = super::extract_gradle_project_dependencies(
            content,
            gradle_dependency_dialect(&manifest),
        );
        let mut records = vec![metadata_record(&project_dir, ":", "project", false)];
        for (index, dependency_path) in dependency_paths.iter().enumerate() {
            let dependency_dir = project_dir.join(format!("dependency-{index}"));
            tokio::fs::create_dir_all(&dependency_dir).await.unwrap();
            let dependency_name = dependency_path.rsplit(':').next().unwrap();
            records.push(metadata_record(
                &dependency_dir,
                dependency_path,
                dependency_name,
                false,
            ));
        }
        create_metadata_gradlew(&project_dir, &records).await;

        let mut finder = finder_with_java_available();
        finder
            .visit(&manifest, &PathBuf::from("project").join(manifest_name))
            .await
            .unwrap();
        let dependencies = finder.projects()[0].dependencies().clone();

        temp_dir.close().unwrap();
        dependencies
    }

    #[test]
    fn test_gradle_wrapper_name_selects_platform_variant() {
        assert_eq!(gradle_wrapper_name(false), "gradlew");
        assert_eq!(gradle_wrapper_name(true), "gradlew.bat");
    }

    #[tokio::test]
    async fn test_find_gradlew_accepts_both_wrapper_filenames_and_respects_bound() {
        for wrapper_name in ["gradlew", "gradlew.bat"] {
            let temp_dir = TempDir::new().unwrap();
            let repo = temp_dir.path().join("repo");
            let project = repo.join("nested");
            fs::create_dir_all(&project).unwrap();
            fs::write(repo.join(wrapper_name), "wrapper").unwrap();
            fs::write(
                temp_dir.path().join(if wrapper_name == "gradlew" {
                    "gradlew.bat"
                } else {
                    "gradlew"
                }),
                "out-of-repo decoy",
            )
            .unwrap();

            let found = find_gradlew_named(&project, 2, wrapper_name)
                .await
                .unwrap()
                .unwrap();

            assert_eq!(found.0, repo.join(wrapper_name));
            assert_eq!(found.1, repo);
        }
    }

    // Both `GradleProjectFinder::new()` and `GradleProjectFinder::default()`
    // must yield the same empty finder that recognizes both Kotlin and
    // Groovy Gradle manifests.
    #[rstest]
    #[case(GradleProjectFinder::new())]
    #[case(GradleProjectFinder::default())]
    fn test_gradle_project_finder_construction(#[case] finder: GradleProjectFinder) {
        assert_eq!(
            finder.project_files(),
            &["build.gradle.kts", "build.gradle"]
        );
        assert_eq!(finder.projects().len(), 0);
    }

    #[derive(Clone, Copy)]
    struct MockGradlew<'a> {
        name: &'a str,
        version: &'a str,
        subprojects: &'a str,
        has_publish_task: bool,
        has_publish_to_maven_local_task: bool,
    }

    impl<'a> MockGradlew<'a> {
        fn package(name: &'a str, version: &'a str) -> Self {
            Self {
                name,
                version,
                subprojects: "[]",
                has_publish_task: true,
                has_publish_to_maven_local_task: true,
            }
        }

        fn workspace(name: &'a str, version: &'a str, subprojects: &'a str) -> Self {
            Self {
                name,
                version,
                subprojects,
                has_publish_task: true,
                has_publish_to_maven_local_task: true,
            }
        }

        fn with_publish_tasks(
            mut self,
            has_publish_task: bool,
            has_publish_to_maven_local_task: bool,
        ) -> Self {
            self.has_publish_task = has_publish_task;
            self.has_publish_to_maven_local_task = has_publish_to_maven_local_task;
            self
        }
    }

    /// Create a mock gradlew in the given directory that emits batched metadata.
    fn create_mock_gradlew(dir: &Path, mock: MockGradlew<'_>) {
        let record = format!(
            "{GRADLE_METADATA_PREFIX}{{\"projectDir\":{},\"projectPath\":\":\",\"name\":{},\"version\":{},\"aggregate\":{},\"hasPublishTask\":{},\"hasPublishToMavenLocalTask\":{}}}",
            json_string(dir.to_string_lossy().as_ref()),
            json_string(mock.name),
            json_string(mock.version),
            mock.subprojects != "[]",
            mock.has_publish_task,
            mock.has_publish_to_maven_local_task,
        );
        if cfg!(windows) {
            fs::write(
                dir.join("gradlew.bat"),
                format!("@echo off\r\necho {record}\r\n"),
            )
            .unwrap();
        } else {
            let gradlew_path = dir.join("gradlew");
            fs::write(
                &gradlew_path,
                format!("#!/bin/sh\nprintf '%s\\n' '{record}'\n"),
            )
            .unwrap();
            #[cfg(unix)]
            make_executable(&gradlew_path);
        }
    }

    fn create_failing_gradlew(dir: &Path) {
        if cfg!(windows) {
            fs::write(
                dir.join("gradlew.bat"),
                "@echo off\n(echo broken build script) >&2\nexit /b 1\n",
            )
            .unwrap();
        } else {
            let gradlew_path = dir.join("gradlew");
            fs::write(
                &gradlew_path,
                "#!/bin/sh\necho 'broken build script' >&2\nexit 1\n",
            )
            .unwrap();
            #[cfg(unix)]
            make_executable(&gradlew_path);
        }
    }

    /// Render `value` as a JSON string literal (quotes included), escaping every
    /// character JSON requires instead of the handful a hand-rolled escaper covers.
    fn json_string(value: &str) -> String {
        serde_json::Value::String(value.to_owned()).to_string()
    }

    fn create_counting_multi_project_gradlew(
        dir: &Path,
        root_project_dir: &Path,
        child_project_dir: &Path,
        child_project_path: &str,
        emit_child_record: bool,
    ) -> PathBuf {
        let invocation_count = dir.join("wrapper-invocations.txt");
        let prefix = GRADLE_METADATA_PREFIX;
        let root_record = format!(
            "{prefix}{{\"projectDir\":{},\"projectPath\":\":\",\"name\":\"root project\",\"version\":\"1.2.3\",\"aggregate\":true,\"hasPublishTask\":true,\"hasPublishToMavenLocalTask\":true}}",
            json_string(root_project_dir.to_string_lossy().as_ref())
        );
        let child_record = format!(
            "{prefix}{{\"projectDir\":{},\"projectPath\":{},\"name\":\"child project\",\"version\":\"2.3.4\",\"aggregate\":false,\"hasPublishTask\":true,\"hasPublishToMavenLocalTask\":true}}",
            json_string(child_project_dir.to_string_lossy().as_ref()),
            json_string(child_project_path),
        );
        let batch_records = if emit_child_record {
            format!(
                "echo {root_record}\r\necho unrelated __CHANGEPACKS_GRADLE_METADATA text\r\necho {child_record}\r\n"
            )
        } else {
            format!("echo {root_record}\r\n")
        };
        let unix_batch_records = if emit_child_record {
            format!(
                "printf '%s\\n' '{root_record}' 'unrelated __CHANGEPACKS_GRADLE_METADATA text' '{child_record}'"
            )
        } else {
            format!("printf '%s\\n' '{root_record}'")
        };

        if cfg!(windows) {
            fs::write(
                dir.join("gradlew.bat"),
                format!(
                    "@echo off\r\n\
                     type nul >\"metadata-command-args.txt\"\r\n\
                     for %%A in (%*) do echo %%~A>>\"metadata-command-args.txt\"\r\n\
                     set count=0\r\n\
                     if exist \"wrapper-invocations.txt\" set /p count=<\"wrapper-invocations.txt\"\r\n\
                     set /a count+=1\r\n\
                     >\"wrapper-invocations.txt\" echo %count%\r\n\
                     {batch_records}\
                     exit /b 0\r\n"
                ),
            )
            .unwrap();
        } else {
            let gradlew_path = dir.join("gradlew");
            fs::write(
                &gradlew_path,
                format!(
                    "#!/bin/sh\n\
                     : > metadata-command-args.txt\n\
                     for arg in \"$@\"; do printf '%s\\n' \"$arg\" >> metadata-command-args.txt; done\n\
                     count=$(cat wrapper-invocations.txt 2>/dev/null || printf 0)\n\
                     count=$((count + 1))\n\
                     printf '%s\\n' \"$count\" > wrapper-invocations.txt\n\
                     {unix_batch_records}\n"
                ),
            )
            .unwrap();
            #[cfg(unix)]
            make_executable(&gradlew_path);
        }

        invocation_count
    }

    fn metadata_record(
        project_dir: &Path,
        project_path: &str,
        name: &str,
        aggregate: bool,
    ) -> String {
        format!(
            "{GRADLE_METADATA_PREFIX}{{\"projectDir\":{},\"projectPath\":{},\"name\":{},\"version\":\"1.0.0\",\"aggregate\":{aggregate},\"hasPublishTask\":true,\"hasPublishToMavenLocalTask\":true}}",
            json_string(project_dir.to_string_lossy().as_ref()),
            json_string(project_path),
            json_string(name),
        )
    }

    async fn create_metadata_gradlew(dir: &Path, records: &[String]) {
        if cfg!(windows) {
            let output = records
                .iter()
                .map(|record| format!("echo {record}\r\n"))
                .collect::<String>();
            tokio::fs::write(
                dir.join("gradlew.bat"),
                format!("@echo off\r\n{output}exit /b 0\r\n"),
            )
            .await
            .unwrap();
        } else {
            let output = records
                .iter()
                .map(|record| format!("printf '%s\\n' '{record}'\n"))
                .collect::<String>();
            let gradlew = dir.join("gradlew");
            tokio::fs::write(&gradlew, format!("#!/bin/sh\n{output}"))
                .await
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                tokio::fs::set_permissions(&gradlew, fs::Permissions::from_mode(0o755))
                    .await
                    .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn test_gradle_metadata_command_disables_lazy_and_cached_configuration() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo");
        let child_dir = repo.join("child");
        fs::create_dir_all(&child_dir).unwrap();
        let root_manifest = repo.join("build.gradle.kts");
        fs::write(&root_manifest, "plugins { java }\n").unwrap();
        create_counting_multi_project_gradlew(&repo, &repo, &child_dir, ":module one", true);

        let mut finder = finder_with_java_available();
        finder
            .visit(&root_manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap();

        let actual = fs::read_to_string(repo.join("metadata-command-args.txt"))
            .unwrap()
            .lines()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        let init_script = actual
            .iter()
            .find(|argument| argument.ends_with(".gradle"))
            .unwrap()
            .clone();
        assert_eq!(
            actual,
            vec![
                "-Dorg.gradle.configureondemand=false".to_string(),
                "-Dorg.gradle.configuration-cache=false".to_string(),
                "--init-script".to_string(),
                init_script,
                "--quiet".to_string(),
                "help".to_string(),
            ]
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_finder_batches_metadata_per_wrapper_root() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo with spaces");
        let child_dir = repo.join("module one");
        fs::create_dir_all(&child_dir).unwrap();
        let root_manifest = repo.join("build.gradle.kts");
        let child_manifest = child_dir.join("build.gradle.kts");
        fs::write(&root_manifest, "plugins { java }\n").unwrap();
        fs::write(&child_manifest, "plugins { java }\n").unwrap();
        let invocation_count =
            create_counting_multi_project_gradlew(&repo, &repo, &child_dir, ":module one", true);

        let mut finder = finder_with_java_available();
        finder
            .visit(&root_manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap();
        finder
            .visit(
                &child_manifest,
                Path::new("module one").join("build.gradle.kts").as_path(),
            )
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);
        let root = projects
            .iter()
            .copied()
            .find(|project| project.name() == Some("root project"))
            .unwrap();
        let child = projects
            .iter()
            .copied()
            .find(|project| project.name() == Some("child project"))
            .unwrap();
        assert!(matches!(root, Project::Workspace(_)));
        assert_eq!(root.version(), Some("1.2.3"));
        assert!(matches!(child, Project::Package(_)));
        assert_eq!(child.version(), Some("2.3.4"));
        assert_eq!(fs::read_to_string(invocation_count).unwrap().trim(), "1");

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_finder_canonicalizes_shared_wrapper_root_once() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo with spaces");
        let alpha_dir = repo.join("alpha");
        let beta_dir = repo.join("beta");
        fs::create_dir_all(&alpha_dir).unwrap();
        fs::create_dir_all(&beta_dir).unwrap();
        let root_manifest = repo.join("build.gradle.kts");
        let alpha_manifest = alpha_dir.join("build.gradle.kts");
        let beta_manifest = beta_dir.join("build.gradle.kts");
        for manifest in [&root_manifest, &alpha_manifest, &beta_manifest] {
            fs::write(manifest, "plugins { java }\n").unwrap();
        }
        create_metadata_gradlew(
            &repo,
            &[
                metadata_record(&repo, ":", "root project", true),
                metadata_record(&alpha_dir, ":alpha", "alpha", false),
                metadata_record(&beta_dir, ":beta", "beta", false),
            ],
        )
        .await;

        let mut finder = finder_with_java_available();
        for (manifest, relative) in [
            (&root_manifest, PathBuf::from("build.gradle.kts")),
            (&alpha_manifest, Path::new("alpha").join("build.gradle.kts")),
            (&beta_manifest, Path::new("beta").join("build.gradle.kts")),
        ] {
            finder.visit(manifest, &relative).await.unwrap();
        }

        // One wrapper root shared by three manifests: exactly one canonicalize
        // result is cached, and it is the canonical repository root.
        let normalized_repo = tokio::fs::canonicalize(&repo).await.unwrap();
        assert_eq!(
            finder.wrapper_dir_canonical.values().collect::<Vec<_>>(),
            vec![&normalized_repo]
        );
        assert_eq!(
            finder.metadata_by_wrapper.keys().collect::<Vec<_>>(),
            vec![&normalized_repo]
        );

        // Both siblings still resolve their own metadata record through that
        // single shared wrapper root.
        let mut names = finder
            .projects()
            .iter()
            .map(|project| project.name().unwrap().to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta", "root project"]);
        for name in ["alpha", "beta"] {
            let project = finder
                .projects()
                .iter()
                .copied()
                .find(|project| project.name() == Some(name))
                .unwrap();
            assert!(matches!(project, Project::Package(_)));
            assert_eq!(project.version(), Some("1.0.0"));
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_finder_publish_uses_metadata_project_path_for_exact_argv() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo with spaces");
        let child_dir = repo.join("generated-backend");
        fs::create_dir_all(&child_dir).unwrap();
        let child_manifest = child_dir.join("build.gradle.kts");
        fs::write(&child_manifest, "plugins { java }\n").unwrap();
        create_counting_multi_project_gradlew(&repo, &repo, &child_dir, ":api", true);

        let mut finder = finder_with_java_available();
        finder
            .visit(
                &child_manifest,
                Path::new("generated-backend/build.gradle.kts"),
            )
            .await
            .unwrap();
        let project = finder.projects()[0];

        let output = project
            .publish(&changepacks_core::Config::default())
            .await
            .unwrap();
        assert!(output.success, "stderr: {}", output.stderr);
        let publish_args = fs::read_to_string(repo.join("metadata-command-args.txt"))
            .unwrap()
            .lines()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(publish_args, [":api:publish"]);

        let dry_run = project
            .dry_run_publish(&changepacks_core::Config::default())
            .await
            .unwrap()
            .unwrap();
        assert!(dry_run.success, "stderr: {}", dry_run.stderr);
        let dry_run_args = fs::read_to_string(repo.join("metadata-command-args.txt"))
            .unwrap()
            .lines()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(dry_run_args.len(), 2, "args: {dry_run_args:?}");
        assert_eq!(dry_run_args[0], ":api:publishToMavenLocal");
        assert!(dry_run_args[1].starts_with("-Dmaven.repo.local="));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_finder_carries_package_publish_task_availability() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("build.gradle.kts");
        fs::write(&manifest, "plugins { java }\n").unwrap();
        create_mock_gradlew(
            temp_dir.path(),
            MockGradlew::package("remote-only", "1.0.0").with_publish_tasks(true, false),
        );

        let mut finder = finder_with_java_available();
        finder
            .visit(&manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap();

        let project = finder.projects()[0];
        assert!(matches!(project, Project::Package(_)));
        assert!(project.is_publishable_by_default());
        assert!(!project.is_dry_run_publishable_by_default());
    }

    #[tokio::test]
    async fn test_gradle_finder_carries_workspace_publish_task_availability() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("build.gradle.kts");
        fs::write(&manifest, "plugins { java }\n").unwrap();
        create_mock_gradlew(
            temp_dir.path(),
            MockGradlew::workspace("local-only", "1.0.0", "[project ':child']")
                .with_publish_tasks(false, true),
        );

        let mut finder = finder_with_java_available();
        finder
            .visit(&manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap();

        let project = finder.projects()[0];
        assert!(matches!(project, Project::Workspace(_)));
        assert!(!project.is_publishable_by_default());
        assert!(project.is_dry_run_publishable_by_default());
    }

    #[tokio::test]
    async fn test_gradle_finder_errors_when_batch_metadata_record_is_missing() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo");
        let child_dir = repo.join("module one");
        fs::create_dir_all(&child_dir).unwrap();
        let root_manifest = repo.join("build.gradle.kts");
        let child_manifest = child_dir.join("build.gradle.kts");
        fs::write(&root_manifest, "plugins { java }\n").unwrap();
        fs::write(&child_manifest, "plugins { java }\n").unwrap();
        create_counting_multi_project_gradlew(&repo, &repo, &child_dir, ":module one", false);

        let mut finder = finder_with_java_available();
        finder
            .visit(&root_manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap();
        let error = finder
            .visit(
                &child_manifest,
                Path::new("module one").join("build.gradle.kts").as_path(),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("missing Gradle metadata record"));
        assert!(
            error
                .to_string()
                .contains(child_dir.to_string_lossy().as_ref())
        );
        assert!(
            error.to_string().contains(
                repo.join(gradle_wrapper_name(cfg!(windows)))
                    .to_string_lossy()
                    .as_ref()
            )
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gradle_publish_task_args_for_root_project() {
        let args = [
            gradle_task_arg_from_project_path(":", "publish"),
            gradle_task_arg_from_project_path(":", "publishToMavenLocal"),
        ];

        assert_eq!(
            args,
            [
                OsString::from("publish"),
                OsString::from("publishToMavenLocal")
            ]
        );
    }

    #[test]
    fn test_gradle_publish_task_args_for_ordinary_nested_project() {
        let args = [
            gradle_task_arg_from_project_path(":libs:core", "publish"),
            gradle_task_arg_from_project_path(":libs:core", "publishToMavenLocal"),
        ];

        assert_eq!(
            args,
            [
                OsString::from(":libs:core:publish"),
                OsString::from(":libs:core:publishToMavenLocal")
            ]
        );
    }

    #[test]
    fn test_gradle_publish_task_args_for_filesystem_remapped_project() {
        let filesystem_path = gradle_subproject_path(Path::new("generated/backend")).unwrap();
        assert_eq!(filesystem_path, "generated:backend");
        assert_ne!(format!(":{filesystem_path}"), ":api");

        let args = [
            gradle_task_arg_from_project_path(":api", "publish"),
            gradle_task_arg_from_project_path(":api", "publishToMavenLocal"),
        ];

        assert_eq!(
            args,
            [
                OsString::from(":api:publish"),
                OsString::from(":api:publishToMavenLocal")
            ]
        );
    }

    #[test]
    fn test_gradle_task_arg_from_project_dir_for_wrapper_root_project() {
        let root = Path::new("/repo");

        let arg = gradle_task_arg_from_project_dir(root, root, "publish").unwrap();

        assert_eq!(arg, OsString::from("publish"));
    }

    #[test]
    fn test_gradle_task_arg_from_project_dir_for_nested_project() {
        let arg =
            gradle_task_arg_from_project_dir(Path::new("/repo/sub"), Path::new("/repo"), "publish")
                .unwrap();

        assert_eq!(arg, OsString::from(":sub:publish"));
    }

    #[test]
    fn test_gradle_task_arg_from_project_dir_for_deeply_nested_project() {
        let arg = gradle_task_arg_from_project_dir(
            Path::new("/repo/libs/core"),
            Path::new("/repo"),
            "publishToMavenLocal",
        )
        .unwrap();

        assert_eq!(arg, OsString::from(":libs:core:publishToMavenLocal"));
    }

    #[test]
    fn test_gradle_task_arg_from_project_dir_rejects_non_descendant_project() {
        let error = gradle_task_arg_from_project_dir(
            Path::new("/elsewhere/sub"),
            Path::new("/repo"),
            "publish",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Failed to compute subproject path"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_gradle_command_spec_matches_active_platform_layout() {
        let gradlew = Path::new("repo").join(if cfg!(windows) {
            "gradlew.bat"
        } else {
            "gradlew"
        });
        let args = vec![OsString::from("--quiet"), OsString::from("help")];

        let spec = GradleCommandSpec::new(&gradlew, Path::new("repo"), args);

        if cfg!(windows) {
            assert_eq!(spec.program, gradlew.as_os_str());
            assert_eq!(
                spec.args,
                vec![OsString::from("--quiet"), OsString::from("help")]
            );
        } else {
            assert_eq!(spec.program, OsString::from("sh"));
            assert_eq!(spec.args[0], gradlew.as_os_str());
            assert_eq!(
                spec.args[1..],
                [OsString::from("--quiet"), OsString::from("help")]
            );
        }
        assert_eq!(spec.current_dir, PathBuf::from("repo"));
    }

    #[tokio::test]
    async fn test_gradle_command_stops_wrapper_when_wait_future_is_dropped() {
        let temp_dir = TempDir::new().unwrap();
        let started = temp_dir.path().join("started.marker");
        let completed = temp_dir.path().join("completed.marker");
        let gradlew = temp_dir.path().join(gradle_wrapper_name(cfg!(windows)));

        if cfg!(windows) {
            fs::write(
                &gradlew,
                "@echo off\r\necho started>started.marker\r\npowershell -NoProfile -Command \"Start-Sleep -Milliseconds 400\"\r\necho completed>completed.marker\r\n",
            )
            .unwrap();
        } else {
            fs::write(
                &gradlew,
                "#!/bin/sh\nprintf started > started.marker\nsleep 0.4\nprintf completed > completed.marker\n",
            )
            .unwrap();
            #[cfg(unix)]
            make_executable(&gradlew);
        }

        let spec = GradleCommandSpec::new(&gradlew, temp_dir.path(), Vec::new());
        let mut command = spec.command();
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let wait_task = tokio::spawn(async move { child.wait().await });

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake Gradle wrapper did not start");

        wait_task.abort();
        let _ = wait_task.await;
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;

        assert!(
            !completed.exists(),
            "dropping a Gradle wait future left its wrapper running"
        );
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_kts_package() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
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

        create_mock_gradlew(&project_dir, MockGradlew::package("myproject", "1.0.0"));

        let mut finder = finder_with_java_available();
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("myproject"));
                assert_eq!(pkg.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_groovy_package() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("groovyproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle");
        fs::write(
            &build_gradle,
            r#"
plugins {
    id 'java'
}

group = 'com.example'
version = '2.0.0'
"#,
        )
        .unwrap();

        create_mock_gradlew(&project_dir, MockGradlew::package("groovyproject", "2.0.0"));

        let mut finder = finder_with_java_available();
        finder
            .visit(&build_gradle, &PathBuf::from("groovyproject/build.gradle"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("groovyproject"));
                assert_eq!(pkg.version(), Some("2.0.0"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_workspace() {
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

        // Mock Gradle metadata reports subprojects (this is what makes it a workspace).
        create_mock_gradlew(
            &project_dir,
            MockGradlew::workspace(
                "multiproject",
                "1.0.0",
                "[project ':subproject1', project ':subproject2']",
            ),
        );

        let mut finder = finder_with_java_available();
        finder
            .visit(
                &build_gradle,
                &PathBuf::from("multiproject/build.gradle.kts"),
            )
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Workspace(ws) => {
                assert_eq!(ws.name(), Some("multiproject"));
                assert_eq!(ws.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_settings_file_does_not_make_workspace() {
        // Regression: settings.gradle.kts presence alone must NOT classify as Workspace.
        // Only evaluated Gradle metadata determines workspace status.
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        // settings.gradle.kts exists and metadata reports no subprojects, so this is a package.
        fs::write(
            project_dir.join("settings.gradle.kts"),
            "rootProject.name = \"myproject\"\n",
        )
        .unwrap();

        create_mock_gradlew(&project_dir, MockGradlew::package("myproject", "1.0.0"));

        let mut finder = finder_with_java_available();
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(_) => {} // correct: subprojects: [] → Package
            _ => panic!("Expected Package, not Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_empty_subprojects_is_package() {
        // A project with gradlew but subprojects: [] is a Package, not Workspace
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("standalone");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        create_mock_gradlew(&project_dir, MockGradlew::package("standalone", "1.0.0"));

        let mut finder = finder_with_java_available();
        finder
            .visit(&build_gradle, &PathBuf::from("standalone/build.gradle.kts"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("standalone"));
            }
            _ => panic!("Expected Package, not Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_non_gradle_file() {
        let temp_dir = TempDir::new().unwrap();
        let other_file = temp_dir.path().join("other.txt");
        fs::write(&other_file, "some content").unwrap();

        let mut finder = finder_with_java_available();
        finder
            .visit(&other_file, &PathBuf::from("other.txt"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        create_mock_gradlew(&project_dir, MockGradlew::package("myproject", "1.0.0"));

        let mut finder = finder_with_java_available();
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        // Visit again - should not add duplicate
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_projects_mut() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        create_mock_gradlew(&project_dir, MockGradlew::package("myproject", "1.0.0"));

        let mut finder = finder_with_java_available();
        finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap();

        let mut_projects = finder.projects_mut();
        assert_eq!(mut_projects.len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_gradlew_in_same_dir() {
        let temp_dir = TempDir::new().unwrap();

        if cfg!(windows) {
            fs::write(temp_dir.path().join("gradlew.bat"), "@echo off").unwrap();
        } else {
            fs::write(temp_dir.path().join("gradlew"), "#!/bin/sh").unwrap();
        }

        // Root project: the build file sits AT the repo root, so `visit`
        // computes `max_depth = 1` and the walk scans only `temp_dir`.
        let result = find_gradlew(temp_dir.path(), 1).await.unwrap();
        assert!(result.is_some());
        let (_, gradlew_dir) = result.unwrap();
        assert_eq!(gradlew_dir, temp_dir.path());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_gradlew_in_parent_dir() {
        let temp_dir = TempDir::new().unwrap();
        let subproject = temp_dir.path().join("libs").join("core");
        fs::create_dir_all(&subproject).unwrap();

        // gradlew at root, not in subproject
        if cfg!(windows) {
            fs::write(temp_dir.path().join("gradlew.bat"), "@echo off").unwrap();
        } else {
            fs::write(temp_dir.path().join("gradlew"), "#!/bin/sh").unwrap();
        }

        // Subproject `libs/core` is two directories below the repo root, so
        // its build file is `libs/core/build.gradle.kts` (3 components) →
        // `max_depth = 3`. The walk scans `libs/core`, `libs`, then `temp_dir`
        // (the repo root), where the wrapper lives.
        let result = find_gradlew(&subproject, 3).await.unwrap();
        assert!(result.is_some());
        let (_, gradlew_dir) = result.unwrap();
        assert_eq!(gradlew_dir, temp_dir.path().to_path_buf());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_gradlew_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let subdir = temp_dir.path().join("no_gradlew_here");
        fs::create_dir_all(&subdir).unwrap();

        // No gradlew in `subdir` or its parent. The walk is now BOUNDED to
        // `max_depth`, so with depth 2 it scans only `subdir` and `temp_dir`
        // and stops — it can no longer climb to the filesystem root and pick
        // up an out-of-repo wrapper, so it reliably returns `None`.
        let result = find_gradlew(&subdir, 2).await.unwrap();
        assert!(result.is_none());

        temp_dir.close().unwrap();
    }

    /// Regression: a decoy `gradlew` ABOVE the repository root must NOT be
    /// discovered (and later executed) when resolving a subproject's wrapper.
    /// The ancestor walk is bounded by `max_depth` (the caller passes
    /// `relative_path.components().count()`), so it scans only the manifest's
    /// in-repo ancestors — down to the repo root — and never reaches the
    /// out-of-repo directory holding the stray wrapper. Project discovery is
    /// git-scoped; a `gradlew` in the user's home dir, the drive root, or a
    /// sibling checkout must not be picked up and run. Against the old
    /// unbounded walk (`loop { current.pop() }` to the filesystem root) this
    /// decoy WAS found, so this test fails there and passes only once the walk
    /// is bounded. Complements `test_find_gradlew_in_parent_dir`, which pins
    /// that an IN-repo ancestor `gradlew` is still found.
    #[tokio::test]
    async fn test_find_gradlew_ignores_gradlew_above_repo_root() {
        let temp_dir = TempDir::new().unwrap();
        // The simulated repo root is a nested subdir; the decoy wrapper lives
        // one level ABOVE it (outside the repo).
        let repo_root = temp_dir.path().join("repo");
        let sub = repo_root.join("sub");
        fs::create_dir_all(&sub).unwrap();

        // Decoy gradlew ABOVE the repo root — must be ignored. (`gradlew.bat`
        // on Windows, `gradlew` elsewhere, matching `create_mock_gradlew`.)
        if cfg!(windows) {
            fs::write(temp_dir.path().join("gradlew.bat"), "@echo off").unwrap();
        } else {
            fs::write(temp_dir.path().join("gradlew"), "#!/bin/sh").unwrap();
        }

        // `relative_path` is repo-root-relative with 2 components
        // (`sub/build.gradle.kts`), so the walk scans `<repo_root>/sub` and
        // `<repo_root>` — never `temp_dir`, where the decoy wrapper lives.
        let result = find_gradlew(&sub, 2).await.unwrap();
        assert!(
            result.is_none(),
            "expected a decoy gradlew above the repo root to be ignored, got {result:?}"
        );

        temp_dir.close().unwrap();
    }

    /// Regression: the DISCOVERY-side wrapper lookup must name the offending
    /// manifest. `find_gradlew` returning `Ok(None)` is covered by
    /// `test_find_gradlew_not_found`, but `GradleProjectFinder::visit` turning
    /// that `None` into a user-facing error was untested, so a message that
    /// dropped the manifest path (as the old hard-coded literal did) went
    /// unnoticed. Asserts BOTH halves of the contract: the stable leading
    /// sentence that `lib.rs` / `package.rs` match with `.contains(..)`, and
    /// the interpolated manifest path.
    #[tokio::test]
    async fn test_gradle_project_finder_visit_missing_wrapper_names_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        // Deliberately NO gradlew/gradlew.bat: the bounded ancestor walk
        // (`max_depth = 2` from `myproject/build.gradle.kts`) scans only
        // `project_dir` and `temp_dir`, both freshly created and wrapper-free.

        let mut finder = finder_with_java_available();
        let error = finder
            .visit(&build_gradle, &PathBuf::from("myproject/build.gradle.kts"))
            .await
            .unwrap_err();

        let flattened = error
            .chain()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ");
        assert!(
            flattened.contains("Gradle wrapper (gradlew) not found"),
            "{flattened}"
        );
        assert!(
            flattened.contains(&build_gradle.display().to_string()),
            "{flattened}"
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gradle_subproject_path_root() {
        assert_eq!(gradle_subproject_path(Path::new("")).unwrap(), "");
    }

    #[test]
    fn test_gradle_subproject_path_single_component() {
        assert_eq!(gradle_subproject_path(Path::new("app")).unwrap(), "app");
    }

    #[test]
    fn test_gradle_subproject_path_nested_unicode() {
        let relative = Path::new("라이브러리").join("핵심");

        assert_eq!(
            gradle_subproject_path(&relative).unwrap(),
            "라이브러리:핵심"
        );
    }

    #[tokio::test]
    async fn test_gradle_finder_resolves_project_path_to_evaluated_name_for_graph_edges() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo");
        let dependency_dir = repo.join("generated-backend");
        tokio::fs::create_dir_all(&dependency_dir).await.unwrap();
        let dependent_manifest = repo.join("build.gradle.kts");
        let dependency_manifest = dependency_dir.join("build.gradle.kts");
        tokio::fs::write(
            &dependent_manifest,
            "dependencies { implementation(project(\":api\")) }\n",
        )
        .await
        .unwrap();
        tokio::fs::write(&dependency_manifest, "plugins { java }\n")
            .await
            .unwrap();
        create_metadata_gradlew(
            &repo,
            &[
                metadata_record(&repo, ":", "service-suite", true),
                metadata_record(&dependency_dir, ":api", "published-api", false),
            ],
        )
        .await;

        let mut finder = finder_with_java_available();
        finder
            .visit(&dependent_manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap();
        finder
            .visit(
                &dependency_manifest,
                Path::new("generated-backend/build.gradle.kts"),
            )
            .await
            .unwrap();

        let projects = finder.projects();
        let dependent = projects
            .iter()
            .copied()
            .find(|project| project.name() == Some("service-suite"))
            .unwrap();
        let dependency = projects
            .iter()
            .copied()
            .find(|project| project.name() == Some("published-api"))
            .unwrap();
        assert_eq!(
            dependent.dependencies(),
            &HashSet::from(["published-api".to_string()])
        );

        let sorted = sort_by_dependencies(vec![dependent, dependency]).unwrap();
        assert_eq!(
            sorted
                .iter()
                .map(|project| project.name().unwrap())
                .collect::<Vec<_>>(),
            vec!["published-api", "service-suite"]
        );

        let mut update_map = HashMap::from([(
            PathBuf::from("generated-backend/build.gradle.kts"),
            (UpdateType::Minor, Vec::new()),
        )]);
        apply_reverse_dependencies(&mut update_map, &[dependency, dependent], &repo).unwrap();
        assert_eq!(
            update_map[&PathBuf::from("build.gradle.kts")].0,
            UpdateType::Patch
        );
    }

    #[tokio::test]
    async fn test_gradle_finder_errors_when_dependency_path_is_missing_from_wrapper_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo");
        tokio::fs::create_dir_all(&repo).await.unwrap();
        let manifest = repo.join("build.gradle.kts");
        tokio::fs::write(
            &manifest,
            "dependencies { implementation(project(\":missing\")) }\n",
        )
        .await
        .unwrap();
        create_metadata_gradlew(
            &repo,
            &[metadata_record(&repo, ":", "service-suite", false)],
        )
        .await;

        let error = finder_with_java_available()
            .visit(&manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap_err();
        let message = error.to_string();

        assert!(message.contains(":missing"), "{message}");
        assert!(message.contains("service-suite"), "{message}");
        assert!(message.contains("gradlew"), "{message}");
    }

    #[tokio::test]
    async fn test_gradle_finder_errors_when_wrapper_metadata_duplicates_project_path() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo");
        let first_dir = repo.join("first");
        let second_dir = repo.join("second");
        tokio::fs::create_dir_all(&first_dir).await.unwrap();
        tokio::fs::create_dir_all(&second_dir).await.unwrap();
        let manifest = repo.join("build.gradle.kts");
        tokio::fs::write(&manifest, "plugins { java }\n")
            .await
            .unwrap();
        create_metadata_gradlew(
            &repo,
            &[
                metadata_record(&repo, ":", "service-suite", true),
                metadata_record(&first_dir, ":api", "first-api", false),
                metadata_record(&second_dir, ":api", "second-api", false),
            ],
        )
        .await;

        let error = finder_with_java_available()
            .visit(&manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap_err();
        let message = error.to_string();

        assert!(message.contains("Duplicate Gradle metadata project path ':api'"));
        assert!(message.contains("first-api"));
        assert!(message.contains("second-api"));
        assert!(message.contains("gradlew"));
    }

    #[tokio::test]
    async fn test_gradle_finder_errors_when_wrapper_metadata_directory_does_not_exist() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo");
        tokio::fs::create_dir_all(&repo).await.unwrap();
        let manifest = repo.join("build.gradle.kts");
        tokio::fs::write(&manifest, "plugins { java }\n")
            .await
            .unwrap();
        let missing_dir = repo.join("never-created");
        create_metadata_gradlew(
            &repo,
            &[
                metadata_record(&repo, ":", "service-suite", true),
                metadata_record(&missing_dir, ":ghost", "ghost-api", false),
            ],
        )
        .await;

        let error = finder_with_java_available()
            .visit(&manifest, Path::new("build.gradle.kts"))
            .await
            .unwrap_err();
        let message = format!("{error:#}");

        assert!(
            message.contains("Failed to normalize Gradle metadata directory"),
            "{message}"
        );
        assert!(message.contains(":ghost"), "{message}");
        assert!(message.contains("never-created"), "{message}");
    }

    #[tokio::test]
    async fn test_gradle_finder_uses_manifest_dialect_for_slashes() {
        let kotlin = dependencies_for_manifest(
            "build.gradle.kts",
            r#"
val first = 12 / 3
dependencies { implementation(project(":real-kotlin")) }
val second = 20 / 4
"#,
        )
        .await;
        assert_eq!(kotlin, HashSet::from(["real-kotlin".to_string()]));

        let groovy = dependencies_for_manifest(
            "build.gradle",
            r#"
def decoy = /project(":slashy-decoy")/
def first = 12 / 3
dependencies { implementation(project(":real-groovy")) }
def second = 20 / 4
"#,
        )
        .await;
        assert_eq!(groovy, HashSet::from(["real-groovy".to_string()]));
    }

    #[tokio::test]
    async fn test_gradle_finder_dependencies_drive_topological_and_reverse_edges() {
        let temp_dir = TempDir::new().unwrap();
        let core_dir = temp_dir.path().join("core");
        let app_dir = temp_dir.path().join("app");
        tokio::fs::create_dir_all(&core_dir).await.unwrap();
        tokio::fs::create_dir_all(&app_dir).await.unwrap();

        let core_manifest = core_dir.join("build.gradle.kts");
        let app_manifest = app_dir.join("build.gradle.kts");
        tokio::fs::write(&core_manifest, "plugins { java }\n")
            .await
            .unwrap();
        tokio::fs::write(
            &app_manifest,
            r#"dependencies {
    implementation(project(configuration = "default", path = ":modules:core"))
}
"#,
        )
        .await
        .unwrap();
        create_metadata_gradlew(
            temp_dir.path(),
            &[
                metadata_record(&core_dir, ":modules:core", "core", false),
                metadata_record(&app_dir, ":app", "app", false),
            ],
        )
        .await;

        let mut finder = finder_with_java_available();
        finder
            .visit(&core_manifest, Path::new("core/build.gradle.kts"))
            .await
            .unwrap();
        finder
            .visit(&app_manifest, Path::new("app/build.gradle.kts"))
            .await
            .unwrap();

        let projects = finder.projects();
        let core = projects
            .iter()
            .copied()
            .find(|project| project.name() == Some("core"))
            .unwrap();
        let app = projects
            .iter()
            .copied()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert_eq!(app.dependencies().len(), 1);
        assert!(app.dependencies().contains("core"));

        let sorted = sort_by_dependencies(vec![app, core]).unwrap();
        assert_eq!(
            sorted
                .iter()
                .map(|project| project.name().unwrap())
                .collect::<Vec<_>>(),
            vec!["core", "app"]
        );

        let mut update_map = HashMap::from([(
            PathBuf::from("core/build.gradle.kts"),
            (UpdateType::Minor, Vec::new()),
        )]);
        apply_reverse_dependencies(&mut update_map, &[core, app], temp_dir.path()).unwrap();
        assert_eq!(
            update_map[&PathBuf::from("app/build.gradle.kts")].0,
            UpdateType::Patch
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_finder_ignores_project_configuration_edge_that_would_form_cycle() {
        let temp_dir = TempDir::new().unwrap();
        let core_dir = temp_dir.path().join("core");
        let app_dir = temp_dir.path().join("app");
        tokio::fs::create_dir_all(&core_dir).await.unwrap();
        tokio::fs::create_dir_all(&app_dir).await.unwrap();

        let core_manifest = core_dir.join("build.gradle.kts");
        let app_manifest = app_dir.join("build.gradle.kts");
        tokio::fs::write(
            &core_manifest,
            r#"project(":app") {
    description = "configuration only"
}
"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            &app_manifest,
            r#"dependencies {
    implementation(project(":core"))
}
"#,
        )
        .await
        .unwrap();
        create_metadata_gradlew(
            temp_dir.path(),
            &[
                metadata_record(&core_dir, ":core", "core", false),
                metadata_record(&app_dir, ":app", "app", false),
            ],
        )
        .await;

        let mut finder = finder_with_java_available();
        finder
            .visit(&core_manifest, Path::new("core/build.gradle.kts"))
            .await
            .unwrap();
        finder
            .visit(&app_manifest, Path::new("app/build.gradle.kts"))
            .await
            .unwrap();

        let projects = finder.projects();
        let core = projects
            .iter()
            .copied()
            .find(|project| project.name() == Some("core"))
            .unwrap();
        let app = projects
            .iter()
            .copied()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert!(core.dependencies().is_empty());
        assert_eq!(app.dependencies(), &HashSet::from(["core".to_string()]));

        let sorted = sort_by_dependencies(vec![app, core]).unwrap();
        assert_eq!(
            sorted
                .iter()
                .map(|project| project.name().unwrap())
                .collect::<Vec<_>>(),
            vec!["core", "app"]
        );

        temp_dir.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_gradle_subproject_path_rejects_non_unicode_component() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let invalid = PathBuf::from(OsString::from_vec(vec![0x66, 0x80, 0x6f]));

        assert!(gradle_subproject_path(&invalid).is_err());
    }

    #[tokio::test]
    async fn test_which_java_in_none() {
        let result = which_java_in(None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_which_java_in_empty() {
        let empty = std::ffi::OsStr::new("");
        let result = which_java_in(Some(empty)).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_which_java_in_with_java_executable() {
        let temp_dir = TempDir::new().unwrap();
        let java_name = if cfg!(windows) { "java.exe" } else { "java" };
        let java_path = temp_dir.path().join(java_name);
        fs::write(&java_path, "").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&java_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let path_var = temp_dir.path().as_os_str();
        let result = which_java_in(Some(path_var)).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().file_name().unwrap(), java_name);

        temp_dir.close().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_which_java_in_rejects_non_executable_file() {
        let temp_dir = TempDir::new().unwrap();
        let java_path = temp_dir.path().join("java");
        fs::write(&java_path, "").unwrap();

        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&java_path, fs::Permissions::from_mode(0o644)).unwrap();

        let result = which_java_in(Some(temp_dir.path().as_os_str()))
            .await
            .unwrap();
        assert!(result.is_none());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_which_java_in_without_java() {
        let temp_dir = TempDir::new().unwrap();
        // Create a directory but no java executable
        fs::create_dir_all(temp_dir.path().join("subdir")).unwrap();

        let path_var = temp_dir.path().as_os_str();
        let result = which_java_in(Some(path_var)).await.unwrap();
        assert!(result.is_none());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_java_home_has_java_rejects_empty_value() {
        assert!(!java_home_has_java(None).await.unwrap());
        assert!(
            !java_home_has_java(Some(std::ffi::OsStr::new("")))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_java_home_has_java_rejects_invalid_home() {
        let temp_dir = TempDir::new().unwrap();
        let invalid_home = temp_dir.path().join("missing-java");
        fs::create_dir_all(&invalid_home).unwrap();

        assert!(
            !java_home_has_java(Some(invalid_home.as_os_str()))
                .await
                .unwrap()
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_java_home_has_java_accepts_bin_java() {
        let temp_dir = TempDir::new().unwrap();
        let java_name = if cfg!(windows) { "java.exe" } else { "java" };
        let java_path = temp_dir.path().join("bin").join(java_name);
        fs::create_dir_all(java_path.parent().unwrap()).unwrap();
        fs::write(&java_path, "").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&java_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        assert!(
            java_home_has_java(Some(temp_dir.path().as_os_str()))
                .await
                .unwrap()
        );

        temp_dir.close().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_java_home_has_java_rejects_non_executable_file() {
        let temp_dir = TempDir::new().unwrap();
        let java_path = temp_dir.path().join("bin").join("java");
        fs::create_dir_all(java_path.parent().unwrap()).unwrap();
        fs::write(&java_path, "").unwrap();

        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&java_path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(
            !java_home_has_java(Some(temp_dir.path().as_os_str()))
                .await
                .unwrap()
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_name_fallback_to_dir() {
        // When gradlew returns name: unspecified, visit() falls back to directory name (line 173).
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("my-fallback-project");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        // Mock gradlew that returns unspecified name (filtered to None)
        create_mock_gradlew(&project_dir, MockGradlew::package("unspecified", "1.0.0"));

        let mut finder = finder_with_java_available();
        finder
            .visit(
                &build_gradle,
                &PathBuf::from("my-fallback-project/build.gradle.kts"),
            )
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                // name fell back to directory name
                assert_eq!(pkg.name(), Some("my-fallback-project"));
                assert_eq!(pkg.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_gradle_project_finder_visit_fails_when_gradlew_fails() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "plugins { id 'java' }").unwrap();

        create_failing_gradlew(&project_dir);

        let mut finder = finder_with_java_available();
        let result = finder
            .visit(&build_gradle, &PathBuf::from("my-project/build.gradle.kts"))
            .await;

        // Visit should propagate the error from batched metadata discovery.
        assert!(result.is_err());
        // No projects should be added when gradlew fails
        assert_eq!(finder.project_count(), 0);

        temp_dir.close().unwrap();
    }
}
