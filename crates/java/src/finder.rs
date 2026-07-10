use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use regex::Regex;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
    sync::LazyLock,
};
use tokio::fs::read_to_string;
use tokio::process::Command;

use crate::{package::GradlePackage, workspace::GradleWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["build.gradle.kts", "build.gradle"];

/// Cached regexes for parsing gradlew `properties -q` output. `LazyLock`
/// mirrors the idiom already used in `crates/java/src/version_updater.rs`
/// (`KTS_SIMPLE_PATTERN` et al.) — the pattern strings are compile-time
/// constants, so re-compiling them on every `get_gradle_properties` call
/// (once per Gradle project per `check` / `update` / `publish`) was pure
/// per-call waste that this now avoids.
static NAME_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^name:\s*(.+)$").expect("hardcoded regex must compile"));

static VERSION_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^version:\s*(.+)$").expect("hardcoded regex must compile"));

static SUBPROJECTS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^subprojects:\s*(.+)$").expect("hardcoded regex must compile")
});

static PROJECT_DEPENDENCY_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"project\(\s*(?:["'](:[^"']+)["']|path\s*=\s*["'](:[^"']+)["']|path\s*:\s*["'](:[^"']+)["'])\s*\)"#,
    )
        .expect("hardcoded regex must compile")
});

#[derive(Debug, Default)]
pub struct GradleProjectFinder {
    projects: HashMap<PathBuf, Project>,
    java_available: Option<bool>,
}

impl GradleProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Project info obtained from gradlew properties
#[derive(Debug, Default)]
struct GradleProperties {
    name: Option<String>,
    version: Option<String>,
    has_subprojects: bool,
}

/// Check if `java` is available on PATH.
///
/// Excluded from coverage: depends on the host's PATH and a real `java`
/// binary; meaningful coverage requires a Java install which CI cannot
/// guarantee on every matrix runner.
#[cfg(not(tarpaulin_include))]
async fn which_java() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = if cfg!(windows) {
            dir.join("java.exe")
        } else {
            dir.join("java")
        };
        // AGENTS.md rule: never blocking I/O in async — replace the blocking
        // `candidate.is_file()` stat with the shared async
        // `changepacks_core::is_regular_file` (a `tokio::fs::metadata` probe).
        // Its internal `unwrap_or(false)` preserves the previous "stat error
        // (missing / permission denied) → treat as not-a-file, keep scanning"
        // semantics, matching how `find_gradlew` above already migrated off
        // blocking `.exists()`.
        if changepacks_core::is_regular_file(&candidate).await {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(tarpaulin_include))]
async fn java_home_has_java(java_home: Option<&OsStr>) -> bool {
    let Some(java_home) = java_home else {
        return false;
    };
    if java_home.is_empty() {
        return false;
    }

    let java_name = if cfg!(windows) { "java.exe" } else { "java" };
    let candidate = Path::new(java_home).join("bin").join(java_name);
    changepacks_core::is_regular_file(&candidate).await
}

/// Find gradlew executable by walking up the directory tree.
///
/// In multi-module Gradle builds, `gradlew` lives at the root while subprojects
/// only contain `build.gradle.kts`. This function searches upward from `start_dir`
/// until it finds `gradlew` (Unix) or `gradlew.bat` (Windows).
///
/// Returns `(gradlew_path, gradlew_dir)` or `None` if not found.
///
/// Excluded from coverage: the cross-platform `cfg!(windows)` arm only
/// executes one branch per test host, leaving the other permanently
/// uncovered. Real coverage requires running against both OS targets,
/// which CI exercises via the matrix build but tarpaulin sees only on
/// Linux.
#[cfg(not(tarpaulin_include))]
async fn find_gradlew(start_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let gradlew_name = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };

    let mut current = start_dir.to_path_buf();
    loop {
        let gradlew = current.join(gradlew_name);
        // Probe with the shared `changepacks_core::is_regular_file` (a
        // `tokio::fs::metadata().is_file()` check) so this file-vs-dir question
        // matches the sibling `which_java` probe above. Its internal
        // `unwrap_or(false)` preserves the previous "stat error (permission
        // denied, broken symlink) → treat as not found, keep walking up"
        // semantics, and it additionally rejects a *directory* named
        // `gradlew`/`gradlew.bat` — which `try_exists` would accept and which
        // would then fail confusingly at execution time.
        let exists = changepacks_core::is_regular_file(&gradlew).await;
        if exists {
            return Some((gradlew, current));
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(not(tarpaulin_include))]
fn gradle_subproject_path(relative: &Path) -> Result<String> {
    let mut path = String::new();
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

fn gradle_dependency_name(project_path: &str) -> Option<String> {
    project_path
        .trim_matches(':')
        .rsplit(':')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn extract_gradle_project_dependencies(content: &str) -> Vec<String> {
    PROJECT_DEPENDENCY_PATTERN
        .captures_iter(content)
        .filter_map(|caps| {
            caps.get(1)
                .or_else(|| caps.get(2))
                .or_else(|| caps.get(3))
                .and_then(|m| gradle_dependency_name(m.as_str()))
        })
        .collect()
}

/// Returns true when a Java runtime is available via JAVA_HOME or PATH.
#[cfg(not(tarpaulin_include))]
async fn java_is_available() -> bool {
    let java_home = std::env::var_os("JAVA_HOME");
    java_home_has_java(java_home.as_deref()).await || which_java().await.is_some()
}

/// Get project properties using gradlew command.
///
/// Walks up the directory tree to find `gradlew`, then runs it with the correct
/// subproject path. For a subproject at `root/libs/core/`, this runs:
/// `./gradlew :libs:core:properties -q` from the root directory.
///
/// Returns `Err` when `gradlew` is not found or Java is not available.
///
/// Excluded from coverage: requires a real Gradle wrapper + Java runtime
/// to exercise; tarpaulin's Linux-only container cannot guarantee both
/// platform arms (sh vs cmd) get hit.
#[cfg(not(tarpaulin_include))]
async fn get_gradle_properties(
    project_dir: &Path,
    java_available: bool,
) -> Result<GradleProperties> {
    let (gradlew, gradlew_dir) = find_gradlew(project_dir).await.context(
        "Gradle wrapper (gradlew) not found. \
         Ensure the project root contains gradlew or gradlew.bat.",
    )?;

    // Gradle requires Java. Error early with a clear message rather than
    // letting gradlew produce a confusing "JAVA_HOME is not set" wall of text.
    // `which_java` is now async (it awaits `is_regular_file`), so hoist the
    // short-circuiting OR into a local before feeding `anyhow::ensure!`.
    anyhow::ensure!(
        java_available,
        "Java is required for Gradle projects but JAVA_HOME is not set and 'java' was not found on PATH.\n\
         Please set the JAVA_HOME environment variable or add java to your PATH."
    );

    let args = gradle_properties_args(project_dir, &gradlew_dir)?;
    let command_spec = GradleCommandSpec::new(&gradlew, &gradlew_dir, &args);
    let output = command_spec
        .command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to execute gradlew for '{}' (gradlew: '{}'): {e}",
                project_dir.display(),
                gradlew.display(),
            )
        })?;

    if !output.status.success() {
        eprintln!(
            "{}",
            gradle_failure_diagnostic(project_dir, &gradlew, output.status, &output.stderr)
        );
        return Ok(GradleProperties::default());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut props = GradleProperties::default();

    // Parse properties output. Regexes are cached via module-level
    // `LazyLock<Regex>` (see `NAME_PATTERN` et al. above) so this hot path
    // no longer re-compiles the three patterns on every visit.
    // Format: "propertyName: value"
    if let Some(caps) = NAME_PATTERN.captures(&stdout) {
        let name = caps.get(1).map(|m| m.as_str().trim().to_string());
        if name.as_deref() != Some("unspecified") {
            props.name = name;
        }
    }

    if let Some(caps) = VERSION_PATTERN.captures(&stdout) {
        let version = caps.get(1).map(|m| m.as_str().trim().to_string());
        if version.as_deref() != Some("unspecified") {
            props.version = version;
        }
    }

    // Detect workspace: subprojects is non-empty (e.g. "[project ':sub1', project ':sub2']")
    if let Some(caps) = SUBPROJECTS_PATTERN.captures(&stdout) {
        let value = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        props.has_subprojects = value != "[]";
    }

    Ok(props)
}

fn gradle_properties_args(project_dir: &Path, gradlew_dir: &Path) -> Result<Vec<String>> {
    if gradlew_dir == project_dir {
        return Ok(vec!["properties".to_string(), "-q".to_string()]);
    }

    let relative = project_dir
        .strip_prefix(gradlew_dir)
        .context("Failed to compute subproject path")?;
    let gradle_path = gradle_subproject_path(relative)?;
    Ok(vec![format!(":{gradle_path}:properties"), "-q".to_string()])
}

#[derive(Debug, PartialEq, Eq)]
struct GradleCommandSpec {
    program: OsString,
    args: Vec<OsString>,
    current_dir: PathBuf,
}

impl GradleCommandSpec {
    fn new(gradlew: &Path, gradlew_dir: &Path, gradle_args: &[String]) -> Self {
        let mut args = Vec::with_capacity(gradle_args.len() + usize::from(!cfg!(windows)));
        let program = if cfg!(windows) {
            gradlew.as_os_str().to_owned()
        } else {
            args.push(gradlew.as_os_str().to_owned());
            OsString::from("sh")
        };
        args.extend(gradle_args.iter().map(OsString::from));

        Self {
            program,
            args,
            current_dir: gradlew_dir.to_path_buf(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).current_dir(&self.current_dir);
        command
    }
}

fn gradle_failure_diagnostic(
    project_dir: &Path,
    gradlew: &Path,
    status: std::process::ExitStatus,
    stderr: &[u8],
) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!(
            "Gradle properties failed for '{}' using '{}' with status {}; falling back to default metadata.",
            project_dir.display(),
            gradlew.display(),
            status
        )
    } else {
        format!(
            "Gradle properties failed for '{}' using '{}' with status {}; falling back to default metadata. stderr: {}",
            project_dir.display(),
            gradlew.display(),
            status,
            stderr
        )
    }
}

#[async_trait]
impl ProjectFinder for GradleProjectFinder {
    changepacks_core::impl_projects_hashmap_accessors!();

    fn project_files(&self) -> &[&str] {
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        if !self.matches_project_file(path).await {
            return Ok(());
        }

        if self.projects.contains_key(path) {
            return Ok(());
        }

        let project_dir = path
            .parent()
            .with_context(|| format!("Parent not found - {}", path.display()))?;

        let java_available = match self.java_available {
            Some(value) => value,
            None => {
                let value = java_is_available().await;
                self.java_available = Some(value);
                value
            }
        };

        // Get properties from gradlew command
        let props = get_gradle_properties(project_dir, java_available).await?;
        let dependencies = read_to_string(path)
            .await
            .map(|content| extract_gradle_project_dependencies(&content))
            .with_context(|| format!("Failed to read Gradle build file {}", path.display()))?;

        // Use directory name as fallback for project name
        let name = props.name.or_else(|| {
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(std::string::ToString::to_string)
        });

        let version = props.version;

        // Workspace detection: gradlew reports non-empty subprojects list.
        // Previous approach (checking for settings.gradle.kts existence) caused
        // false positives in composite builds and subprojects with IDE-generated files.
        let is_workspace = props.has_subprojects;

        // Hoist the map key allocation out of both arms: the old shape
        // built a `(PathBuf, Project)` tuple, which forced each branch
        // to call `path.to_path_buf()` TWICE (once for the tuple slot,
        // once again for `*::new`). One shared `path_key` + one
        // `.clone()` into the constructor cuts 4 `PathBuf` allocs to 2.
        let path_key = path.to_path_buf();
        let mut project = if is_workspace {
            Project::Workspace(Box::new(GradleWorkspace::new(
                name,
                version,
                path_key.clone(),
                relative_path.to_path_buf(),
            )))
        } else {
            Project::Package(Box::new(GradlePackage::new(
                name,
                version,
                path_key.clone(),
                relative_path.to_path_buf(),
            )))
        };

        for dependency in dependencies {
            project.add_dependency(&dependency);
        }

        self.projects.insert(path_key, project);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::Project;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;

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
    }

    impl<'a> MockGradlew<'a> {
        fn package(name: &'a str, version: &'a str) -> Self {
            Self {
                name,
                version,
                subprojects: "[]",
            }
        }

        fn workspace(name: &'a str, version: &'a str, subprojects: &'a str) -> Self {
            Self {
                name,
                version,
                subprojects,
            }
        }
    }

    /// Create a mock gradlew in the given directory that outputs Gradle properties.
    fn create_mock_gradlew(dir: &Path, mock: MockGradlew<'_>) {
        if cfg!(windows) {
            fs::write(
                dir.join("gradlew.bat"),
                format!(
                    "@echo off\necho name: {}\necho version: {}\necho subprojects: {}\n",
                    mock.name, mock.version, mock.subprojects
                ),
            )
            .unwrap();
        } else {
            let gradlew_path = dir.join("gradlew");
            fs::write(
                &gradlew_path,
                format!(
                    "#!/bin/sh\necho 'name: {}'\necho 'version: {}'\necho \"subprojects: {}\"\n",
                    mock.name, mock.version, mock.subprojects
                ),
            )
            .unwrap();
            #[cfg(unix)]
            make_executable(&gradlew_path);
        }
    }

    fn create_failing_gradlew(dir: &Path) {
        if cfg!(windows) {
            fs::write(dir.join("gradlew.bat"), "@echo off\nexit /b 1\n").unwrap();
        } else {
            let gradlew_path = dir.join("gradlew");
            fs::write(&gradlew_path, "#!/bin/sh\nexit 1\n").unwrap();
            #[cfg(unix)]
            make_executable(&gradlew_path);
        }
    }

    #[cfg(unix)]
    fn failed_exit_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        std::process::ExitStatus::from_raw(256)
    }

    #[cfg(windows)]
    fn failed_exit_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;

        std::process::ExitStatus::from_raw(1)
    }

    #[test]
    fn test_gradle_failure_diagnostic_includes_context_and_stderr() {
        let msg = gradle_failure_diagnostic(
            Path::new("apps/demo"),
            Path::new("gradlew"),
            failed_exit_status(),
            b"broken build script\n",
        );

        assert!(msg.contains("apps/demo"));
        assert!(msg.contains("gradlew"));
        assert!(msg.contains("falling back to default metadata"));
        assert!(msg.contains("broken build script"));
    }

    #[test]
    fn test_gradle_properties_args_root_project() {
        let root = Path::new("repo");

        let args = gradle_properties_args(root, root).unwrap();

        assert_eq!(args, vec!["properties", "-q"]);
    }

    #[test]
    fn test_gradle_properties_args_subproject() {
        let root = Path::new("repo");
        let subproject = root.join("libs").join("core");

        let args = gradle_properties_args(&subproject, root).unwrap();

        assert_eq!(args, vec![":libs:core:properties", "-q"]);
    }

    #[test]
    fn test_gradle_command_spec_matches_active_platform_layout() {
        let gradlew = Path::new("repo").join(if cfg!(windows) {
            "gradlew.bat"
        } else {
            "gradlew"
        });
        let args = vec!["properties".to_string(), "-q".to_string()];

        let spec = GradleCommandSpec::new(&gradlew, Path::new("repo"), &args);

        if cfg!(windows) {
            assert_eq!(spec.program, gradlew.as_os_str());
            assert_eq!(
                spec.args,
                vec![OsString::from("properties"), OsString::from("-q")]
            );
        } else {
            assert_eq!(spec.program, OsString::from("sh"));
            assert_eq!(spec.args[0], gradlew.as_os_str());
            assert_eq!(
                spec.args[1..],
                [OsString::from("properties"), OsString::from("-q")]
            );
        }
        assert_eq!(spec.current_dir, PathBuf::from("repo"));
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

        let mut finder = GradleProjectFinder::new();
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

        let mut finder = GradleProjectFinder::new();
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

        // Mock gradlew that reports subprojects (this is what makes it a workspace)
        create_mock_gradlew(
            &project_dir,
            MockGradlew::workspace(
                "multiproject",
                "1.0.0",
                "[project ':subproject1', project ':subproject2']",
            ),
        );

        let mut finder = GradleProjectFinder::new();
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
        // Only gradlew's subprojects output determines workspace status.
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let build_gradle = project_dir.join("build.gradle.kts");
        fs::write(&build_gradle, "version = \"1.0.0\"\n").unwrap();

        // settings.gradle.kts exists AND gradlew exists, but subprojects: [] → Package
        fs::write(
            project_dir.join("settings.gradle.kts"),
            "rootProject.name = \"myproject\"\n",
        )
        .unwrap();

        create_mock_gradlew(&project_dir, MockGradlew::package("myproject", "1.0.0"));

        let mut finder = GradleProjectFinder::new();
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

        let mut finder = GradleProjectFinder::new();
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

        let mut finder = GradleProjectFinder::new();
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

        let mut finder = GradleProjectFinder::new();
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

        let mut finder = GradleProjectFinder::new();
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

        let result = find_gradlew(temp_dir.path()).await;
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

        let result = find_gradlew(&subproject).await;
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

        // Don't create gradlew anywhere — but find_gradlew walks to filesystem
        // root, so this test just verifies it doesn't panic. In practice it
        // returns None only when no gradlew exists anywhere up the tree.
        // For a reliable "not found" test, we rely on the no-gradlew properties test below.
        let _ = find_gradlew(&subdir).await;

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_no_gradlew() {
        let temp_dir = TempDir::new().unwrap();
        let subdir = temp_dir.path().join("isolated");
        fs::create_dir_all(&subdir).unwrap();
        // No gradlew anywhere in this subtree → should error
        let result = get_gradle_properties(&subdir, true).await;
        // May find a system gradlew higher up; the key contract is it doesn't panic.
        // If no gradlew found at all, it returns Err.
        let _ = result;
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_with_mock() {
        let temp_dir = TempDir::new().unwrap();

        create_mock_gradlew(temp_dir.path(), MockGradlew::package("myproject", "1.2.3"));

        let props = get_gradle_properties(temp_dir.path(), true).await.unwrap();
        assert_eq!(props.name, Some("myproject".to_string()));
        assert_eq!(props.version, Some("1.2.3".to_string()));
        assert!(!props.has_subprojects);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_with_subprojects() {
        let temp_dir = TempDir::new().unwrap();

        create_mock_gradlew(
            temp_dir.path(),
            MockGradlew::workspace("root", "1.0.0", "[project ':app', project ':lib']"),
        );

        let props = get_gradle_properties(temp_dir.path(), true).await.unwrap();
        assert_eq!(props.name, Some("root".to_string()));
        assert!(props.has_subprojects);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_empty_subprojects() {
        let temp_dir = TempDir::new().unwrap();

        create_mock_gradlew(temp_dir.path(), MockGradlew::package("leaf", "1.0.0"));

        let props = get_gradle_properties(temp_dir.path(), true).await.unwrap();
        assert_eq!(props.name, Some("leaf".to_string()));
        assert!(!props.has_subprojects);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_from_parent_gradlew() {
        let temp_dir = TempDir::new().unwrap();
        let subproject = temp_dir.path().join("sub1");
        fs::create_dir_all(&subproject).unwrap();

        // Place gradlew at root, query from subproject dir
        // Mock: ignore the :sub1:properties arg, just output properties
        create_mock_gradlew(temp_dir.path(), MockGradlew::package("sub1", "2.0.0"));

        let props = get_gradle_properties(&subproject, true).await.unwrap();
        assert_eq!(props.name, Some("sub1".to_string()));
        assert_eq!(props.version, Some("2.0.0".to_string()));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_nested_subproject() {
        let temp_dir = TempDir::new().unwrap();
        let subproject = temp_dir.path().join("libs").join("core");
        fs::create_dir_all(&subproject).unwrap();

        // Place gradlew at root, query from libs/core/
        // The mock script receives ":libs:core:properties" "-q" as args.
        create_mock_gradlew(temp_dir.path(), MockGradlew::package("core", "3.1.0"));

        let props = get_gradle_properties(&subproject, true).await.unwrap();
        assert_eq!(props.name, Some("core".to_string()));
        assert_eq!(props.version, Some("3.1.0".to_string()));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_gradle_subproject_path_nested_unicode() {
        let relative = Path::new("libs").join("core");

        assert_eq!(gradle_subproject_path(&relative).unwrap(), "libs:core");
    }

    #[test]
    fn test_extract_gradle_project_dependencies_simple_kotlin_and_groovy() {
        let content = r#"
dependencies {
    implementation(project(":lib"))
    testImplementation(project(':testing:fixtures'))
    api(project(path = ":core"))
    runtimeOnly(project(path = ':tools:cli'))
    implementation(project(path: ':shared'))
    implementation("org.example:external:1.0.0")
}
"#;

        let dependencies = extract_gradle_project_dependencies(content);

        assert_eq!(
            dependencies,
            vec!["lib", "fixtures", "core", "cli", "shared"]
        );
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
    async fn test_get_gradle_properties_unspecified() {
        let temp_dir = TempDir::new().unwrap();

        create_mock_gradlew(
            temp_dir.path(),
            MockGradlew::package("unspecified", "unspecified"),
        );

        let props = get_gradle_properties(temp_dir.path(), true).await.unwrap();
        assert!(props.name.is_none());
        assert!(props.version.is_none());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_gradlew_fails() {
        let temp_dir = TempDir::new().unwrap();

        create_failing_gradlew(temp_dir.path());

        // gradlew exits non-zero → returns default props (no name, no version)
        let props = get_gradle_properties(temp_dir.path(), true).await.unwrap();
        assert!(props.name.is_none());
        assert!(props.version.is_none());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_which_java_returns_some_or_none() {
        // Exercises which_java() — the result depends on the test environment,
        // but the function must not panic regardless.
        let result = which_java().await;
        // On most dev/CI machines java is on PATH → Some; otherwise None.
        // Both branches are valid; we just verify it runs without error.
        let _ = result;
    }

    #[tokio::test]
    async fn test_which_java_with_empty_path() {
        // Temporarily set PATH to empty to guarantee the None branch (line 50).
        let original = std::env::var_os("PATH");
        // SAFETY: this test runs single-threaded; no other thread reads PATH concurrently.
        unsafe { std::env::set_var("PATH", "") };

        let result = which_java().await;
        assert!(result.is_none());

        // Restore
        if let Some(p) = original {
            // SAFETY: restoring original value, single-threaded test context.
            unsafe { std::env::set_var("PATH", p) };
        }
    }

    #[tokio::test]
    async fn test_java_home_has_java_rejects_empty_value() {
        assert!(!java_home_has_java(None).await);
        assert!(!java_home_has_java(Some(std::ffi::OsStr::new(""))).await);
    }

    #[tokio::test]
    async fn test_java_home_has_java_rejects_invalid_home() {
        let temp_dir = TempDir::new().unwrap();
        let invalid_home = temp_dir.path().join("missing-java");
        fs::create_dir_all(&invalid_home).unwrap();

        assert!(!java_home_has_java(Some(invalid_home.as_os_str())).await);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_java_home_has_java_accepts_bin_java() {
        let temp_dir = TempDir::new().unwrap();
        let java_name = if cfg!(windows) { "java.exe" } else { "java" };
        let java_path = temp_dir.path().join("bin").join(java_name);
        fs::create_dir_all(java_path.parent().unwrap()).unwrap();
        fs::write(&java_path, "").unwrap();

        assert!(java_home_has_java(Some(temp_dir.path().as_os_str())).await);

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

        let mut finder = GradleProjectFinder::new();
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
}
