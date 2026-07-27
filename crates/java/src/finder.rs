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

use crate::{package::GradlePackage, workspace::GradleWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["build.gradle.kts", "build.gradle"];

macro_rules! gradle_metadata_prefix {
    () => {
        "__CHANGEPACKS_GRADLE_METADATA_V1__"
    };
}

const GRADLE_METADATA_PREFIX: &str = gradle_metadata_prefix!();

const GRADLE_METADATA_INIT_SCRIPT: &str = concat!(
    r#"import groovy.json.JsonOutput

gradle.projectsEvaluated { evaluatedGradle ->
    evaluatedGradle.rootProject.allprojects { project ->
        def record = [
            projectDir: project.projectDir.toPath().toAbsolutePath().normalize().toString(),
            projectPath: project.path,
            name: project.name,
            version: project.version == null ? null : project.version.toString(),
            aggregate: !project.childProjects.isEmpty(),
            hasPublishTask: project.tasks.findByName("publish") != null,
            hasPublishToMavenLocalTask: project.tasks.findByName("publishToMavenLocal") != null
        ]
        println(""#,
    gradle_metadata_prefix!(),
    r#"" + JsonOutput.toJson(record))
    }
}
"#
);

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
}

impl GradleProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Project info obtained from batched Gradle metadata.
#[derive(Clone, Debug, Default)]
struct GradleProperties {
    name: Option<String>,
    version: Option<String>,
    has_subprojects: bool,
    has_publish_task: bool,
    has_publish_to_maven_local_task: bool,
}

#[derive(Clone, Debug)]
struct GradleMetadataRecord {
    project_dir: PathBuf,
    project_path: String,
    properties: GradleProperties,
}

#[derive(Debug)]
struct GradleWrapperMetadata {
    by_project_dir: HashMap<PathBuf, GradleMetadataRecord>,
    project_names_by_path: HashMap<String, String>,
}

/// Removes `field` from a Gradle metadata record and converts it with `extract`.
///
/// The three typed accessors below only differ in the accepted
/// `serde_json::Value` variants and in the type name quoted by the error, so the
/// shared missing-field and wrong-type reporting lives here. `extract` hands the
/// value back through `Err` when it does not match, which keeps the rejected
/// value available for the message without formatting it on the success path.
fn metadata_field<T>(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    expected: &str,
    extract: impl FnOnce(serde_json::Value) -> std::result::Result<T, serde_json::Value>,
) -> Result<T> {
    match fields.remove(field) {
        Some(value) => extract(value).map_err(|value| {
            anyhow::anyhow!("Gradle metadata field '{field}' must be {expected}, got {value:?}")
        }),
        None => Err(anyhow::anyhow!(
            "Gradle metadata record is missing required field '{field}'"
        )),
    }
}

fn required_metadata_string(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String> {
    metadata_field(fields, field, "a string", |value| match value {
        serde_json::Value::String(value) => Ok(value),
        other => Err(other),
    })
}

fn optional_metadata_string(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<String>> {
    metadata_field(fields, field, "a string or null", |value| match value {
        serde_json::Value::String(value) => Ok(Some(value)),
        serde_json::Value::Null => Ok(None),
        other => Err(other),
    })
}

fn required_metadata_bool(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<bool> {
    metadata_field(fields, field, "a boolean", |value| match value {
        serde_json::Value::Bool(value) => Ok(value),
        other => Err(other),
    })
}

fn normalized_gradle_property(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| value != "unspecified")
}

fn parse_gradle_metadata_record(json: &str) -> Result<GradleMetadataRecord> {
    let mut fields: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(json).context("invalid Gradle metadata JSON object")?;
    let project_dir = required_metadata_string(&mut fields, "projectDir")?;
    anyhow::ensure!(
        !project_dir.is_empty(),
        "Gradle metadata field 'projectDir' must not be empty"
    );
    let project_path = required_metadata_string(&mut fields, "projectPath")?;
    anyhow::ensure!(
        project_path.starts_with(':'),
        "Gradle metadata field 'projectPath' must be a qualified Gradle path"
    );
    let name = required_metadata_string(&mut fields, "name")?;
    let version = optional_metadata_string(&mut fields, "version")?;
    let has_subprojects = required_metadata_bool(&mut fields, "aggregate")?;
    let has_publish_task = required_metadata_bool(&mut fields, "hasPublishTask")?;
    let has_publish_to_maven_local_task =
        required_metadata_bool(&mut fields, "hasPublishToMavenLocalTask")?;

    Ok(GradleMetadataRecord {
        project_dir: PathBuf::from(project_dir),
        project_path,
        properties: GradleProperties {
            name: normalized_gradle_property(Some(name)),
            version: normalized_gradle_property(version),
            has_subprojects,
            has_publish_task,
            has_publish_to_maven_local_task,
        },
    })
}

fn parse_gradle_metadata_records(output: &str) -> Result<Vec<GradleMetadataRecord>> {
    output
        .lines()
        .enumerate()
        .filter_map(|(line_index, line)| {
            line.strip_prefix(GRADLE_METADATA_PREFIX)
                .map(|record| (line_index, record))
        })
        .map(|(line_index, record)| {
            parse_gradle_metadata_record(record).map_err(|error| {
                anyhow::anyhow!(
                    "malformed Gradle metadata record at line {}: {error:#}",
                    line_index + 1
                )
            })
        })
        .collect()
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
///
fn gradle_wrapper_name(windows: bool) -> &'static str {
    if windows { "gradlew.bat" } else { "gradlew" }
}

async fn find_gradlew(start_dir: &Path, max_depth: usize) -> Result<Option<(PathBuf, PathBuf)>> {
    find_gradlew_named(start_dir, max_depth, gradle_wrapper_name(cfg!(windows))).await
}

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
    let (gradlew, gradlew_dir) = find_gradlew(project_dir, max_depth).await?.context(
        "Gradle wrapper (gradlew) not found. \
         Ensure the project root contains gradlew or gradlew.bat.",
    )?;
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

fn is_gradle_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

fn is_gradle_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

#[derive(Clone, Copy, Debug, Default)]
struct SignificantBytes {
    before_last: Option<u8>,
    last: Option<u8>,
}

impl SignificantBytes {
    fn push(&mut self, byte: u8) {
        self.before_last = self.last;
        self.last = Some(byte);
    }

    fn is_member_access(self) -> bool {
        self.last == Some(b'.')
            || matches!(
                (self.before_last, self.last),
                (Some(b'.'), Some(b'&' | b'@')) | (Some(b':'), Some(b':'))
            )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum GradlePreviousToken {
    #[default]
    StatementStart,
    ExpressionStart,
    ExpressionEnd,
}

#[derive(Clone, Copy, Debug, Default)]
struct GradleLexState {
    significant: SignificantBytes,
    previous: GradlePreviousToken,
}

impl GradleLexState {
    fn slashy_allowed(self) -> bool {
        self.previous != GradlePreviousToken::ExpressionEnd
    }

    fn is_member_access(self) -> bool {
        self.significant.is_member_access()
    }

    fn allows_recovery_candidate(self) -> bool {
        self.previous == GradlePreviousToken::ExpressionEnd
    }

    fn mark_literal(&mut self) {
        self.significant.push(b'v');
        self.previous = GradlePreviousToken::ExpressionEnd;
    }

    fn mark_identifier(&mut self, identifier: &[u8]) {
        if let Some(&last) = identifier.last() {
            self.significant.push(last);
        }
        self.previous = if groovy_identifier_allows_expression(identifier) {
            GradlePreviousToken::ExpressionStart
        } else {
            GradlePreviousToken::ExpressionEnd
        };
    }

    fn mark_byte(&mut self, byte: u8) {
        self.significant.push(byte);
        self.previous = match byte {
            b';' => GradlePreviousToken::StatementStart,
            b'(' | b'[' | b'{' | b',' | b':' | b'=' | b'?' | b'+' | b'-' | b'*' | b'/' | b'%'
            | b'!' | b'&' | b'|' | b'^' | b'<' | b'>' | b'~' => {
                GradlePreviousToken::ExpressionStart
            }
            _ => GradlePreviousToken::ExpressionEnd,
        };
    }

    fn mark_statement_start(&mut self) {
        self.previous = GradlePreviousToken::StatementStart;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GradleIdentifierContext {
    Dependencies,
    DependencyHandlerMember,
    Other,
}

#[derive(Clone, Copy, Debug)]
struct GradlePendingIdentifier {
    context: GradleIdentifierContext,
    begins_dependency_statement: bool,
}

#[derive(Clone, Copy, Debug)]
enum GradleDependencyDelimiter {
    Parenthesis { dependency_declaration: bool },
    Bracket,
    Block { dependencies: bool },
}

#[derive(Debug)]
struct GradleDependencyContext {
    delimiters: Vec<GradleDependencyDelimiter>,
    pending_identifier: Option<GradlePendingIdentifier>,
    dependency_handler_member_access: bool,
    dependency_statement: bool,
    statement_start: bool,
}

impl Default for GradleDependencyContext {
    fn default() -> Self {
        Self {
            delimiters: Vec::new(),
            pending_identifier: None,
            dependency_handler_member_access: false,
            dependency_statement: false,
            statement_start: true,
        }
    }
}

impl GradleDependencyContext {
    fn is_directly_in_dependencies_block(&self) -> bool {
        self.delimiters
            .iter()
            .rposition(|delimiter| {
                matches!(
                    delimiter,
                    GradleDependencyDelimiter::Block { dependencies: true }
                )
            })
            .is_some_and(|position| position + 1 == self.delimiters.len())
    }

    fn allows_project_dependency(&self) -> bool {
        self.delimiters.iter().any(|delimiter| {
            matches!(
                delimiter,
                GradleDependencyDelimiter::Parenthesis {
                    dependency_declaration: true
                }
            )
        }) || (self.is_directly_in_dependencies_block() && self.dependency_statement)
    }

    fn mark_identifier(&mut self, identifier: &[u8], member_access: bool) {
        let begins_dependency_statement =
            self.is_directly_in_dependencies_block() && self.statement_start && !member_access;
        if begins_dependency_statement {
            self.dependency_statement = true;
        }

        let context = if self.dependency_handler_member_access {
            GradleIdentifierContext::DependencyHandlerMember
        } else if identifier == b"dependencies" {
            GradleIdentifierContext::Dependencies
        } else {
            GradleIdentifierContext::Other
        };
        self.pending_identifier = Some(GradlePendingIdentifier {
            context,
            begins_dependency_statement,
        });
        self.dependency_handler_member_access = false;
        self.statement_start = false;
    }

    fn mark_literal(&mut self) {
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.statement_start = false;
    }

    fn open_parenthesis(&mut self) {
        let dependency_handler_call = self.pending_identifier.is_some_and(|identifier| {
            identifier.context == GradleIdentifierContext::DependencyHandlerMember
        });
        let dependency_declaration = dependency_handler_call
            || (self.is_directly_in_dependencies_block() && self.dependency_statement);
        self.delimiters
            .push(GradleDependencyDelimiter::Parenthesis {
                dependency_declaration,
            });
        self.mark_expression();
    }

    fn open_bracket(&mut self) {
        self.delimiters.push(GradleDependencyDelimiter::Bracket);
        self.mark_expression();
    }

    fn open_block(&mut self) {
        let dependencies = self
            .pending_identifier
            .is_some_and(|identifier| identifier.context == GradleIdentifierContext::Dependencies);
        self.delimiters
            .push(GradleDependencyDelimiter::Block { dependencies });
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.dependency_statement = false;
        self.statement_start = true;
    }

    fn close_parenthesis(&mut self) {
        if matches!(
            self.delimiters.last(),
            Some(GradleDependencyDelimiter::Parenthesis { .. })
        ) {
            self.delimiters.pop();
        }
        self.mark_expression();
    }

    fn close_bracket(&mut self) {
        if matches!(
            self.delimiters.last(),
            Some(GradleDependencyDelimiter::Bracket)
        ) {
            self.delimiters.pop();
        }
        self.mark_expression();
    }

    fn close_block(&mut self) {
        if matches!(
            self.delimiters.last(),
            Some(GradleDependencyDelimiter::Block { .. })
        ) {
            self.delimiters.pop();
        }
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.dependency_statement = false;
        self.statement_start = false;
    }

    fn mark_byte(&mut self, byte: u8) {
        if byte == b'.' {
            self.dependency_handler_member_access =
                self.pending_identifier.is_some_and(|identifier| {
                    identifier.context == GradleIdentifierContext::Dependencies
                });
            self.pending_identifier = None;
            self.statement_start = false;
            return;
        }

        if byte == b';' {
            self.pending_identifier = None;
            self.dependency_handler_member_access = false;
            self.dependency_statement = false;
            self.statement_start = true;
            return;
        }

        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        if matches!(byte, b'=' | b':') {
            self.dependency_statement = false;
        }
        self.statement_start = false;
    }

    fn mark_line_break(&mut self) {
        let continues_identifier = self.pending_identifier.is_some_and(|identifier| {
            identifier.begins_dependency_statement
                || identifier.context != GradleIdentifierContext::Other
        });
        if !continues_identifier {
            self.pending_identifier = None;
            self.dependency_statement = false;
        }
        self.dependency_handler_member_access = false;
        self.statement_start = true;
    }

    fn mark_expression(&mut self) {
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.statement_start = false;
    }

    fn recover_after_malformed_call(&mut self) {
        if let Some(dependencies) = self.delimiters.iter().rposition(|delimiter| {
            matches!(
                delimiter,
                GradleDependencyDelimiter::Block { dependencies: true }
            )
        }) {
            self.delimiters.truncate(dependencies + 1);
        } else {
            self.delimiters.clear();
        }
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.dependency_statement = false;
        self.statement_start = true;
    }
}

fn gradle_identifier_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while bytes
        .get(end)
        .copied()
        .is_some_and(is_gradle_identifier_byte)
    {
        end += 1;
    }
    end
}

fn groovy_identifier_allows_expression(identifier: &[u8]) -> bool {
    matches!(
        identifier,
        b"assert" | b"case" | b"in" | b"instanceof" | b"new" | b"return" | b"throw" | b"yield"
    )
}

fn quoted_gradle_literal_end(
    bytes: &[u8],
    start: usize,
    triple: bool,
    dialect: GradleDependencyDialect,
) -> Option<usize> {
    let quote = *bytes.get(start)?;
    if !matches!(quote, b'\'' | b'"') {
        return None;
    }

    let mut cursor = start + if triple { 3 } else { 1 };
    while cursor < bytes.len() {
        if triple && bytes[cursor] == quote {
            let run_start = cursor;
            while bytes.get(cursor) == Some(&quote) {
                cursor += 1;
            }
            let quote_count = cursor - run_start;
            if quote_count >= 3 {
                let mut backslash_count = 0usize;
                let mut previous = run_start;
                while previous > start + 2 && bytes[previous - 1] == b'\\' {
                    previous -= 1;
                    backslash_count += 1;
                }
                if dialect == GradleDependencyDialect::Kotlin || backslash_count.is_multiple_of(2) {
                    return Some(cursor);
                }
            }
            continue;
        }

        match bytes[cursor] {
            b'\\' if !triple || dialect == GradleDependencyDialect::Groovy => {
                cursor = (cursor + 2).min(bytes.len());
            }
            byte if byte == quote => return Some(cursor + 1),
            b'\r' | b'\n' if !triple => return None,
            _ => cursor += 1,
        }
    }
    None
}

fn slashy_gradle_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'/' => return Some(cursor + 1),
            _ => cursor += 1,
        }
    }
    None
}

fn dollar_slashy_gradle_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 2;
    while cursor < bytes.len() {
        if bytes.get(cursor..cursor + 2) == Some(b"/$") {
            return Some(cursor + 2);
        }
        cursor += if bytes[cursor] == b'$' && cursor + 1 < bytes.len() {
            2
        } else {
            1
        };
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GradleLiteralScan {
    NotLiteral,
    Complete(usize),
    Unterminated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GradleDependencyDialect {
    Kotlin,
    Groovy,
}

fn gradle_dependency_dialect(manifest_path: &Path) -> GradleDependencyDialect {
    if manifest_path
        .file_name()
        .is_some_and(|name| name == "build.gradle.kts")
    {
        GradleDependencyDialect::Kotlin
    } else {
        GradleDependencyDialect::Groovy
    }
}

fn scan_gradle_literal(
    bytes: &[u8],
    start: usize,
    dialect: GradleDependencyDialect,
    slashy_allowed: bool,
) -> GradleLiteralScan {
    if dialect == GradleDependencyDialect::Groovy && bytes.get(start..start + 2) == Some(b"$/") {
        return dollar_slashy_gradle_literal_end(bytes, start)
            .map_or(GradleLiteralScan::Unterminated, GradleLiteralScan::Complete);
    }

    if let Some(quote @ (b'\'' | b'"')) = bytes.get(start).copied() {
        let triple = bytes.get(start + 1) == Some(&quote) && bytes.get(start + 2) == Some(&quote);
        return quoted_gradle_literal_end(bytes, start, triple, dialect)
            .map_or(GradleLiteralScan::Unterminated, GradleLiteralScan::Complete);
    }

    if dialect == GradleDependencyDialect::Groovy
        && slashy_allowed
        && bytes.get(start) == Some(&b'/')
    {
        // Groovy `/` is slashy only where the bounded previous-token state
        // allows an expression to start. A slash after a number, identifier,
        // literal, or closing delimiter remains division.
        return slashy_gradle_literal_end(bytes, start)
            .map_or(GradleLiteralScan::NotLiteral, GradleLiteralScan::Complete);
    }

    GradleLiteralScan::NotLiteral
}

fn skip_gradle_comment(
    bytes: &[u8],
    start: usize,
    dialect: GradleDependencyDialect,
) -> Option<usize> {
    match (bytes.get(start), bytes.get(start + 1)) {
        (Some(b'/'), Some(b'/')) => {
            let mut cursor = start + 2;
            while cursor < bytes.len() && !matches!(bytes[cursor], b'\r' | b'\n') {
                cursor += 1;
            }
            Some(cursor)
        }
        (Some(b'/'), Some(b'*')) => {
            let mut cursor = start + 2;
            let mut depth = 1usize;
            while cursor + 1 < bytes.len() {
                match (bytes[cursor], bytes[cursor + 1]) {
                    (b'/', b'*') if dialect == GradleDependencyDialect::Kotlin => {
                        depth += 1;
                        cursor += 2;
                    }
                    (b'*', b'/') => {
                        depth -= 1;
                        cursor += 2;
                        if depth == 0 {
                            return Some(cursor);
                        }
                    }
                    _ => cursor += 1,
                }
            }
            Some(bytes.len())
        }
        _ => None,
    }
}

fn skip_gradle_trivia(
    bytes: &[u8],
    mut cursor: usize,
    end: usize,
    dialect: GradleDependencyDialect,
) -> usize {
    loop {
        while cursor < end && bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if cursor >= end {
            return end;
        }
        let Some(next) = skip_gradle_comment(bytes, cursor, dialect) else {
            return cursor;
        };
        cursor = next.min(end);
    }
}

fn looks_like_statement_call(bytes: &[u8], start: usize, dialect: GradleDependencyDialect) -> bool {
    let mut cursor = skip_gradle_trivia(bytes, start, bytes.len(), dialect);
    if !bytes
        .get(cursor)
        .copied()
        .is_some_and(is_gradle_identifier_start)
    {
        return false;
    }
    while bytes
        .get(cursor)
        .copied()
        .is_some_and(is_gradle_identifier_byte)
    {
        cursor += 1;
    }
    cursor = skip_gradle_trivia(bytes, cursor, bytes.len(), dialect);
    bytes.get(cursor) == Some(&b'(')
}

fn gradle_line_break_end(bytes: &[u8], cursor: usize) -> Option<usize> {
    match bytes.get(cursor) {
        Some(b'\r') if bytes.get(cursor + 1) == Some(&b'\n') => Some(cursor + 2),
        Some(b'\r' | b'\n') => Some(cursor + 1),
        _ => None,
    }
}

fn verified_blank_line_resume(
    bytes: &[u8],
    cursor: usize,
    dialect: GradleDependencyDialect,
) -> Option<usize> {
    let mut next = gradle_line_break_end(bytes, cursor)?;
    while matches!(bytes.get(next), Some(b' ' | b'\t')) {
        next += 1;
    }
    next = gradle_line_break_end(bytes, next)?;
    let resume = skip_gradle_trivia(bytes, next, bytes.len(), dialect);
    looks_like_statement_call(bytes, resume, dialect).then_some(resume)
}

#[derive(Debug)]
enum GradleCallScan {
    Complete {
        end: usize,
        arguments: Vec<(usize, usize)>,
    },
    Malformed {
        resume: usize,
    },
}

fn gradle_quarantine_resume(
    bytes: &[u8],
    start: usize,
    dialect: GradleDependencyDialect,
    mut expected_closers: Vec<u8>,
) -> usize {
    let mut cursor = start;
    let mut lexical = GradleLexState::default();

    while cursor < bytes.len() {
        if let Some(next) = skip_gradle_comment(bytes, cursor, dialect) {
            cursor = next;
            continue;
        }

        match scan_gradle_literal(bytes, cursor, dialect, lexical.slashy_allowed()) {
            GradleLiteralScan::Complete(next) => {
                cursor = next;
                lexical.mark_literal();
                continue;
            }
            GradleLiteralScan::Unterminated => return bytes.len(),
            GradleLiteralScan::NotLiteral => {}
        }

        if bytes
            .get(cursor)
            .copied()
            .is_some_and(is_gradle_identifier_start)
        {
            let end = gradle_identifier_end(bytes, cursor);
            lexical.mark_identifier(&bytes[cursor..end]);
            cursor = end;
            continue;
        }

        match bytes[cursor] {
            b'\r' | b'\n' => {
                if expected_closers.is_empty()
                    && let Some(resume) = verified_blank_line_resume(bytes, cursor, dialect)
                {
                    return resume;
                }
                cursor = gradle_line_break_end(bytes, cursor).unwrap_or(cursor + 1);
                if !matches!(expected_closers.last(), Some(b')' | b']')) {
                    lexical.mark_statement_start();
                }
            }
            byte @ (b'(' | b'[' | b'{') => {
                expected_closers.push(match byte {
                    b'(' => b')',
                    b'[' => b']',
                    b'{' => b'}',
                    _ => unreachable!(),
                });
                lexical.mark_byte(byte);
                cursor += 1;
            }
            byte @ (b')' | b']' | b'}') => {
                if expected_closers.last() == Some(&byte) {
                    expected_closers.pop();
                }
                lexical.mark_byte(byte);
                cursor += 1;
            }
            byte if byte.is_ascii_whitespace() => cursor += 1,
            byte => {
                lexical.mark_byte(byte);
                cursor += 1;
            }
        }
    }

    bytes.len()
}

fn malformed_gradle_call(
    bytes: &[u8],
    quarantine_start: usize,
    dialect: GradleDependencyDialect,
    expected_closers: Vec<u8>,
) -> GradleCallScan {
    GradleCallScan::Malformed {
        resume: gradle_quarantine_resume(bytes, quarantine_start, dialect, expected_closers),
    }
}

fn scan_gradle_call(bytes: &[u8], open: usize, dialect: GradleDependencyDialect) -> GradleCallScan {
    let mut cursor = open + 1;
    let mut expected_closers = vec![b')'];
    let mut arguments = Vec::new();
    let mut argument_start = cursor;
    let mut lexical = GradleLexState::default();
    lexical.mark_byte(b'(');
    let mut recovery_candidate = None;

    while cursor < bytes.len() {
        if let Some(next) = skip_gradle_comment(bytes, cursor, dialect) {
            cursor = next;
            continue;
        }

        match scan_gradle_literal(bytes, cursor, dialect, lexical.slashy_allowed()) {
            GradleLiteralScan::Complete(next) => {
                cursor = next;
                lexical.mark_literal();
                continue;
            }
            GradleLiteralScan::Unterminated => {
                if let Some(resume) = recovery_candidate {
                    return GradleCallScan::Malformed { resume };
                }
                return malformed_gradle_call(bytes, cursor, dialect, expected_closers);
            }
            GradleLiteralScan::NotLiteral => {}
        }

        if bytes
            .get(cursor)
            .copied()
            .is_some_and(is_gradle_identifier_start)
        {
            let end = gradle_identifier_end(bytes, cursor);
            lexical.mark_identifier(&bytes[cursor..end]);
            cursor = end;
            continue;
        }

        match bytes[cursor] {
            b'\r' | b'\n' => {
                if expected_closers.len() == 1
                    && lexical.allows_recovery_candidate()
                    && let Some(resume) = verified_blank_line_resume(bytes, cursor, dialect)
                {
                    recovery_candidate.get_or_insert(resume);
                }
                cursor = gradle_line_break_end(bytes, cursor).unwrap_or(cursor + 1);
            }
            byte @ (b'(' | b'[' | b'{') => {
                expected_closers.push(match byte {
                    b'(' => b')',
                    b'[' => b']',
                    b'{' => b'}',
                    _ => unreachable!(),
                });
                lexical.mark_byte(byte);
                cursor += 1;
            }
            byte @ (b')' | b']' | b'}') => {
                if expected_closers.last() != Some(&byte) {
                    if let Some(resume) = recovery_candidate {
                        return GradleCallScan::Malformed { resume };
                    }
                    return malformed_gradle_call(bytes, cursor + 1, dialect, expected_closers);
                }
                if expected_closers.len() == 1 {
                    arguments.push((argument_start, cursor));
                    return GradleCallScan::Complete {
                        end: cursor + 1,
                        arguments,
                    };
                }
                expected_closers.pop();
                lexical.mark_byte(byte);
                cursor += 1;
            }
            b',' if expected_closers.len() == 1 => {
                arguments.push((argument_start, cursor));
                argument_start = cursor + 1;
                lexical.mark_byte(b',');
                cursor += 1;
            }
            byte if byte.is_ascii_whitespace() => cursor += 1,
            byte => {
                lexical.mark_byte(byte);
                cursor += 1;
            }
        }
    }

    if let Some(resume) = recovery_candidate {
        return GradleCallScan::Malformed { resume };
    }
    malformed_gradle_call(bytes, bytes.len(), dialect, expected_closers)
}

fn gradle_assignment(
    content: &str,
    start: usize,
    end: usize,
    dialect: GradleDependencyDialect,
) -> Option<(&str, usize)> {
    let bytes = content.as_bytes();
    let mut cursor = skip_gradle_trivia(bytes, start, end, dialect);
    let identifier_start = cursor;
    if !bytes
        .get(cursor)
        .copied()
        .is_some_and(is_gradle_identifier_start)
    {
        return None;
    }
    while cursor < end && is_gradle_identifier_byte(bytes[cursor]) {
        cursor += 1;
    }
    let identifier = content.get(identifier_start..cursor)?;
    cursor = skip_gradle_trivia(bytes, cursor, end, dialect);
    if !matches!(bytes.get(cursor), Some(b'=' | b':')) {
        return None;
    }
    cursor = skip_gradle_trivia(bytes, cursor + 1, end, dialect);
    Some((identifier, cursor))
}

fn plain_gradle_project_path(
    content: &str,
    start: usize,
    end: usize,
    dialect: GradleDependencyDialect,
) -> Option<&str> {
    let bytes = content.as_bytes();
    let string_start = skip_gradle_trivia(bytes, start, end, dialect);
    let quote = *bytes.get(string_start)?;
    if !matches!(quote, b'\'' | b'"')
        || (bytes.get(string_start + 1) == Some(&quote)
            && bytes.get(string_start + 2) == Some(&quote))
    {
        return None;
    }

    let string_end = quoted_gradle_literal_end(bytes, string_start, false, dialect)?;
    if string_end > end || skip_gradle_trivia(bytes, string_end, end, dialect) != end {
        return None;
    }

    let project_path = content.get(string_start + 1..string_end - 1)?;
    (!project_path.contains(['\\', '$']) && project_path.starts_with(':')).then_some(project_path)
}

fn gradle_dependency_from_arguments<'a>(
    content: &'a str,
    arguments: &[(usize, usize)],
    dialect: GradleDependencyDialect,
) -> Option<&'a str> {
    let bytes = content.as_bytes();
    let mut candidate_count = 0usize;
    let mut project_path = None;

    for (index, &(start, end)) in arguments.iter().enumerate() {
        let argument_start = skip_gradle_trivia(bytes, start, end, dialect);
        if argument_start == end {
            continue;
        }

        if let Some((name, value_start)) = gradle_assignment(content, start, end, dialect) {
            if name != "path" {
                continue;
            }
            candidate_count += 1;
            if candidate_count > 1 {
                return None;
            }
            project_path = plain_gradle_project_path(content, value_start, end, dialect);
            continue;
        }

        let looks_positional =
            index == 0 || matches!(bytes.get(argument_start), Some(b'\'' | b'"' | b'/' | b'$'));
        if looks_positional {
            candidate_count += 1;
            if candidate_count > 1 || index != 0 {
                return None;
            }
            project_path = plain_gradle_project_path(content, argument_start, end, dialect);
        }
    }

    (candidate_count == 1).then_some(project_path).flatten()
}

fn extract_gradle_project_dependencies(
    content: &str,
    dialect: GradleDependencyDialect,
) -> Vec<&str> {
    const PROJECT_CALL: &[u8] = b"project";

    let bytes = content.as_bytes();
    let mut dependencies = Vec::new();
    let mut cursor = 0usize;
    let mut lexical = GradleLexState::default();
    let mut dependency_context = GradleDependencyContext::default();
    let mut continuation_group_depth = 0usize;

    while cursor < bytes.len() {
        if let Some(next) = skip_gradle_comment(bytes, cursor, dialect) {
            cursor = next;
            continue;
        }

        match scan_gradle_literal(bytes, cursor, dialect, lexical.slashy_allowed()) {
            GradleLiteralScan::Complete(next) => {
                cursor = next;
                lexical.mark_literal();
                dependency_context.mark_literal();
                continue;
            }
            GradleLiteralScan::Unterminated => break,
            GradleLiteralScan::NotLiteral => {}
        }

        let token_end = cursor + PROJECT_CALL.len();
        let is_project_call = bytes
            .get(cursor..)
            .is_some_and(|rest| rest.starts_with(PROJECT_CALL))
            && cursor
                .checked_sub(1)
                .and_then(|previous| bytes.get(previous))
                .is_none_or(|byte| !is_gradle_identifier_byte(*byte))
            && bytes
                .get(token_end)
                .is_none_or(|byte| !is_gradle_identifier_byte(*byte));

        if is_project_call {
            let open = skip_gradle_trivia(bytes, token_end, bytes.len(), dialect);
            if bytes.get(open) == Some(&b'(') {
                let qualified = lexical.is_member_access();
                match scan_gradle_call(bytes, open, dialect) {
                    GradleCallScan::Complete { end, arguments } => {
                        if dependency_context.allows_project_dependency()
                            && !qualified
                            && let Some(dependency) =
                                gradle_dependency_from_arguments(content, &arguments, dialect)
                        {
                            dependencies.push(dependency);
                        }
                        cursor = end;
                        lexical.mark_byte(b')');
                        dependency_context.mark_expression();
                    }
                    GradleCallScan::Malformed { resume } => {
                        cursor = resume.max(open + 1);
                        lexical = GradleLexState::default();
                        dependency_context.recover_after_malformed_call();
                        continuation_group_depth = 0;
                    }
                }
                continue;
            }
        }

        if bytes
            .get(cursor)
            .copied()
            .is_some_and(is_gradle_identifier_start)
        {
            let end = gradle_identifier_end(bytes, cursor);
            dependency_context.mark_identifier(&bytes[cursor..end], lexical.is_member_access());
            lexical.mark_identifier(&bytes[cursor..end]);
            cursor = end;
            continue;
        }

        match bytes[cursor] {
            b'(' => {
                continuation_group_depth += 1;
                dependency_context.open_parenthesis();
                lexical.mark_byte(b'(');
            }
            b'[' => {
                continuation_group_depth += 1;
                dependency_context.open_bracket();
                lexical.mark_byte(b'[');
            }
            b'{' => {
                dependency_context.open_block();
                lexical.mark_byte(b'{');
            }
            b')' => {
                continuation_group_depth = continuation_group_depth.saturating_sub(1);
                dependency_context.close_parenthesis();
                lexical.mark_byte(b')');
            }
            b']' => {
                continuation_group_depth = continuation_group_depth.saturating_sub(1);
                dependency_context.close_bracket();
                lexical.mark_byte(b']');
            }
            b'}' => {
                dependency_context.close_block();
                lexical.mark_byte(b'}');
            }
            b'\r' | b'\n' if continuation_group_depth == 0 => {
                dependency_context.mark_line_break();
                lexical.mark_statement_start();
            }
            byte if byte.is_ascii_whitespace() => {}
            byte => {
                dependency_context.mark_byte(byte);
                lexical.mark_byte(byte);
            }
        }
        cursor += 1;
    }

    dependencies
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

fn gradle_metadata_args(init_script_path: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-Dorg.gradle.configureondemand=false"),
        OsString::from("-Dorg.gradle.configuration-cache=false"),
        OsString::from("--init-script"),
        init_script_path.as_os_str().to_owned(),
        OsString::from("--quiet"),
        OsString::from("help"),
    ]
}

async fn get_gradle_metadata(
    gradlew: &Path,
    gradlew_dir: &Path,
    java_available: bool,
) -> Result<GradleWrapperMetadata> {
    anyhow::ensure!(
        java_available,
        "Java is required for Gradle projects but JAVA_HOME is not set and 'java' was not found on PATH.\n\
         Please set the JAVA_HOME environment variable or add java to your PATH."
    );

    let init_script = tempfile::Builder::new()
        .prefix("changepacks-gradle-metadata-")
        .suffix(".gradle")
        .tempfile()
        .context("Failed to create temporary Gradle metadata init script")?;
    let init_script_path = init_script.path().to_path_buf();
    tokio::fs::write(&init_script_path, GRADLE_METADATA_INIT_SCRIPT)
        .await
        .with_context(|| {
            format!(
                "Failed to write temporary Gradle metadata init script '{}'",
                init_script_path.display()
            )
        })?;

    let args = gradle_metadata_args(&init_script_path);
    let command_spec = GradleCommandSpec::new(gradlew, gradlew_dir, args);
    let output_result = command_spec.command().output().await;
    let cleanup_failure = init_script.close().err().map(|error| {
        format!(
            "failed to remove temporary Gradle metadata init script '{}': {error}",
            init_script_path.display()
        )
    });
    let cleanup_suffix = cleanup_failure
        .as_deref()
        .map_or_else(String::new, |failure| format!("; additionally, {failure}"));
    let output = match output_result {
        Ok(output) => output,
        Err(error) => {
            return Err(anyhow::anyhow!(
                "Failed to execute Gradle metadata discovery for wrapper root '{}' (gradlew: '{}'): {error}{cleanup_suffix}",
                gradlew_dir.display(),
                gradlew.display(),
            ));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();
        return Err(anyhow::anyhow!(
            "Gradle metadata discovery failed for wrapper root '{}' using '{}' with status {}{}{}",
            gradlew_dir.display(),
            gradlew.display(),
            output.status,
            if stderr_trimmed.is_empty() {
                String::new()
            } else {
                format!("; stderr: {stderr_trimmed}")
            },
            cleanup_suffix
        ));
    }

    if let Some(cleanup_failure) = cleanup_failure {
        return Err(anyhow::anyhow!(
            "Temporary Gradle metadata init-script cleanup failed: {cleanup_failure}"
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let records = parse_gradle_metadata_records(&stdout).map_err(|error| {
        anyhow::anyhow!(
            "Failed to parse Gradle metadata emitted by '{}' for wrapper root '{}': {error:#}",
            gradlew.display(),
            gradlew_dir.display()
        )
    })?;
    let mut by_project_dir = HashMap::with_capacity(records.len());
    let mut project_names_by_path = HashMap::with_capacity(records.len());
    for record in records {
        let normalized_dir = tokio::fs::canonicalize(&record.project_dir)
            .await
            .with_context(|| {
                format!(
                    "Failed to normalize Gradle metadata directory '{}' for project '{}' emitted by '{}'",
                    record.project_dir.display(),
                    record.project_path,
                    gradlew.display()
                )
            })?;
        let project_path = record.project_path.clone();
        let project_name = record
            .properties
            .name
            .clone()
            .or_else(|| {
                normalized_dir
                    .file_name()
                    .and_then(OsStr::to_str)
                    .map(str::to_owned)
            })
            .with_context(|| {
                format!(
                    "Gradle metadata project '{}' from '{}' has no usable evaluated project name",
                    project_path,
                    gradlew.display()
                )
            })?;
        if let Some(previous_name) =
            project_names_by_path.insert(project_path.clone(), project_name.clone())
        {
            return Err(anyhow::anyhow!(
                "Duplicate Gradle metadata project path '{}' from '{}': projects '{}' and '{}'",
                project_path,
                gradlew.display(),
                previous_name,
                project_name
            ));
        }
        if let Some(previous) = by_project_dir.insert(normalized_dir.clone(), record) {
            return Err(anyhow::anyhow!(
                "Duplicate Gradle metadata records for normalized directory '{}' from '{}': projects '{}' and '{}'",
                normalized_dir.display(),
                gradlew.display(),
                previous.project_path,
                project_path
            ));
        }
    }

    Ok(GradleWrapperMetadata {
        by_project_dir,
        project_names_by_path,
    })
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

#[derive(Debug)]
struct GradleCommandSpec {
    program: OsString,
    args: Vec<OsString>,
    current_dir: PathBuf,
}

impl GradleCommandSpec {
    fn new(gradlew: &Path, gradlew_dir: &Path, gradle_args: Vec<OsString>) -> Self {
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

    fn command(&self) -> Command {
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
        if !self.matches_project_file(path).await? {
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

        // Bound the gradlew search to the repository root: `relative_path` is
        // the build file's path relative to the git repo root, so its component
        // count equals the number of directories from `project_dir` up to and
        // INCLUDING the repo root (root project: `build.gradle.kts` → count 1 →
        // check `project_dir` only). This stops the ancestor walk at the repo
        // boundary so an out-of-repo `gradlew` is never discovered or executed.
        // Mirrors the C# finder's `is_workspace` bound.
        let max_depth = relative_path.components().count();

        let (gradlew, gradlew_dir) = find_gradlew(project_dir, max_depth).await?.context(
            "Gradle wrapper (gradlew) not found. \
             Ensure the project root contains gradlew or gradlew.bat.",
        )?;
        let normalized_wrapper_dir =
            tokio::fs::canonicalize(&gradlew_dir)
                .await
                .with_context(|| {
                    format!(
                        "Failed to normalize Gradle wrapper root '{}' for '{}'",
                        gradlew_dir.display(),
                        path.display()
                    )
                })?;

        if !self
            .metadata_by_wrapper
            .contains_key(&normalized_wrapper_dir)
        {
            let metadata = get_gradle_metadata(&gradlew, &gradlew_dir, java_available).await?;
            self.metadata_by_wrapper
                .insert(normalized_wrapper_dir.clone(), metadata);
        }

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
            .get(&normalized_wrapper_dir)
            .with_context(missing_metadata_context)?;
        let metadata = wrapper_metadata
            .by_project_dir
            .get(&normalized_project_dir)
            .with_context(missing_metadata_context)?;
        let project_path = metadata.project_path.clone();
        let GradleProperties {
            name,
            version,
            has_subprojects,
            has_publish_task,
            has_publish_to_maven_local_task,
        } = metadata.properties.clone();

        // Use directory name as fallback for project name
        let name = name.or_else(|| {
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(std::string::ToString::to_string)
        });

        let project_names_by_path = &wrapper_metadata.project_names_by_path;
        let dependency_names = dependencies
            .iter()
            .map(|dependency_path| {
                project_names_by_path
                    .get(*dependency_path)
                    .with_context(|| {
                        format!(
                            "Gradle dependency project path '{}' declared by project '{}' (Gradle path '{}', manifest '{}') is missing from metadata emitted by wrapper '{}'",
                            dependency_path,
                            name.as_deref().unwrap_or("<unnamed>"),
                            project_path,
                            path.display(),
                            gradlew.display()
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?;

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

    fn extract_gradle_project_dependencies(content: &str) -> Vec<&str> {
        super::extract_gradle_project_dependencies(content, GradleDependencyDialect::Groovy)
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

    fn json_string(value: &str) -> String {
        let mut escaped = String::with_capacity(value.len() + 2);
        escaped.push('"');
        for character in value.chars() {
            match character {
                '"' => escaped.push_str("\\\""),
                '\\' => escaped.push_str("\\\\"),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                character => escaped.push(character),
            }
        }
        escaped.push('"');
        escaped
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

    #[test]
    fn test_gradle_metadata_init_script_reports_publish_task_availability() {
        assert!(
            GRADLE_METADATA_INIT_SCRIPT
                .contains("hasPublishTask: project.tasks.findByName(\"publish\") != null")
        );
        assert!(GRADLE_METADATA_INIT_SCRIPT.contains(
            "hasPublishToMavenLocalTask: project.tasks.findByName(\"publishToMavenLocal\") != null"
        ));
    }

    #[test]
    fn test_parse_gradle_metadata_records_handles_spaces_unicode_and_unrelated_output() {
        let output = concat!(
            "Gradle configuration output\n",
            "unrelated __CHANGEPACKS_GRADLE_METADATA_V1__{not a record}\n",
            "__CHANGEPACKS_GRADLE_METADATA_V1__{\"projectDir\":\"C:\\\\repo with spaces\\\\모듈\",",
            "\"projectPath\":\":module one:유니코드\",\"name\":\"이름 with spaces\",",
            "\"version\":\"1.2.3-β\",\"aggregate\":false,",
            "\"hasPublishTask\":true,\"hasPublishToMavenLocalTask\":false}\n",
            "> Task :help\n",
        );

        let records = parse_gradle_metadata_records(output).unwrap();

        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(
            record.project_dir,
            PathBuf::from(r"C:\repo with spaces\모듈")
        );
        assert_eq!(record.project_path, ":module one:유니코드");
        assert_eq!(record.properties.name.as_deref(), Some("이름 with spaces"));
        assert_eq!(record.properties.version.as_deref(), Some("1.2.3-β"));
        assert!(!record.properties.has_subprojects);
        assert!(record.properties.has_publish_task);
        assert!(!record.properties.has_publish_to_maven_local_task);
    }

    #[test]
    fn test_parse_gradle_metadata_records_ignores_unknown_protocol_versions() {
        let output = concat!(
            "__CHANGEPACKS_GRADLE_METADATA_V0__{not current metadata}\n",
            "__CHANGEPACKS_GRADLE_METADATA_V2__{not current metadata}\n",
            "ordinary __CHANGEPACKS_GRADLE_METADATA_V1__{not line-prefixed metadata}\n",
        );

        assert!(parse_gradle_metadata_records(output).unwrap().is_empty());
    }

    #[test]
    fn test_parse_gradle_metadata_records_rejects_missing_publish_task_field() {
        let output = concat!(
            "__CHANGEPACKS_GRADLE_METADATA_V1__{\"projectDir\":\"/repo\",",
            "\"projectPath\":\":\",\"name\":\"root\",\"version\":\"1.0.0\",",
            "\"aggregate\":false,\"hasPublishToMavenLocalTask\":true}\n",
        );

        let error = parse_gradle_metadata_records(output).unwrap_err();

        assert!(error.to_string().contains("line 1"));
        assert!(error.to_string().contains("hasPublishTask"));
    }

    #[test]
    fn test_parse_gradle_metadata_records_rejects_non_boolean_local_publish_task_field() {
        let output = concat!(
            "__CHANGEPACKS_GRADLE_METADATA_V1__{\"projectDir\":\"/repo\",",
            "\"projectPath\":\":\",\"name\":\"root\",\"version\":\"1.0.0\",",
            "\"aggregate\":false,\"hasPublishTask\":true,",
            "\"hasPublishToMavenLocalTask\":\"yes\"}\n",
        );

        let error = parse_gradle_metadata_records(output).unwrap_err();

        assert!(error.to_string().contains("line 1"));
        assert!(error.to_string().contains("hasPublishToMavenLocalTask"));
        assert!(error.to_string().contains("must be a boolean"));
    }

    #[test]
    fn test_metadata_field_accessors_report_exact_type_and_missing_wording() {
        let mut fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"name":7,"version":true,"aggregate":"yes"}"#).unwrap();

        assert_eq!(
            required_metadata_string(&mut fields, "name")
                .unwrap_err()
                .to_string(),
            "Gradle metadata field 'name' must be a string, got Number(7)"
        );
        assert_eq!(
            optional_metadata_string(&mut fields, "version")
                .unwrap_err()
                .to_string(),
            "Gradle metadata field 'version' must be a string or null, got Bool(true)"
        );
        assert_eq!(
            required_metadata_bool(&mut fields, "aggregate")
                .unwrap_err()
                .to_string(),
            "Gradle metadata field 'aggregate' must be a boolean, got String(\"yes\")"
        );
        assert_eq!(
            required_metadata_string(&mut fields, "name")
                .unwrap_err()
                .to_string(),
            "Gradle metadata record is missing required field 'name'"
        );
        assert!(fields.is_empty());
    }

    #[test]
    fn test_parse_gradle_metadata_records_rejects_malformed_prefixed_record() {
        let output = concat!(
            "ordinary output\n",
            "__CHANGEPACKS_GRADLE_METADATA_V1__{\"projectDir\":\"/repo\",\"aggregate\":wat}\n",
        );

        let error = parse_gradle_metadata_records(output).unwrap_err();

        assert!(error.to_string().contains("line 2"));
        assert!(
            error
                .to_string()
                .contains("malformed Gradle metadata record")
        );
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
            vec![
                ":lib",
                ":testing:fixtures",
                ":core",
                ":tools:cli",
                ":shared"
            ]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_with_additional_arguments() {
        let content = r#"
dependencies {
    implementation(project(":libraries:공통", configuration = "shadow"))
    api(project(
        path = ":services:인증",
        configuration = "default",
    ))
    runtimeOnly(project(
        path: ':도구:cli',
        configuration: 'runtimeElements'
    ))
}
"#;

        let dependencies = extract_gradle_project_dependencies(content);

        assert_eq!(
            dependencies,
            vec![":libraries:공통", ":services:인증", ":도구:cli"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_ignores_nested_and_unrelated_expressions() {
        let content = r#"
dependencies {
    implementation(project(findProject(":nested-expression"), configuration = "default"))
    api(project(path = resolvePath(":computed-path"), configuration = "default"))
    runtimeOnly(project(path: choosePath(':computed-groovy'), configuration: 'default'))
    implementation(project(":real", configuration = provider(project(":nested-decoy"))))
    implementation(notproject(":identifier-suffix"))
    implementation(projectFactory(":factory"))
    implementation("org.example:external:1.0.0")
}

val rendered = "project(\":string-decoy\", configuration = \"default\")"
// project(":line-comment-decoy", configuration = "default")
/* project(path = ":block-comment-decoy", configuration = "default") */
"#;

        let dependencies = extract_gradle_project_dependencies(content);

        assert_eq!(dependencies, vec![":real"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_requires_dependency_declaration_context() {
        let content = r#"
project(":configured") {
    description = "project configuration, not a dependency"
}
val kotlinAssignment = project(path = ":assigned-kotlin")
def groovyAssignment = project(path: ':assigned-groovy')

dependencies {
    implementation(project(":real"))
    runtimeOnly(platform(project(path = ":nested-real")))
    testImplementation project(path: ':command-real')
}
dependencies.add("compileOnly", project(":direct-add-real"))
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":real", ":nested-real", ":command-real", ":direct-add-real"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_skips_gradle_literals_and_dynamic_paths() {
        let content = r##"
val kotlinRaw = """quoted " text project(":triple-double-decoy") """
def groovyTriple = '''quoted ' text project(':triple-single-decoy') '''
def groovySlashy = /text project(":slashy-decoy") text/
def groovyDollarSlashy = $/text project(":dollar-slashy-decoy") text/$

dependencies {
    implementation(project(":plain:유니코드"))
    api(project(":escaped\\path"))
    runtimeOnly(project(":$interpolated"))
    testImplementation(project(":${computed}"))
}
"##;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":plain:유니코드"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_skips_comments_after_division() {
        let content = r#"
val first = numerator / denominator // project(":line-comment-decoy")
val second = numerator / denominator /* project(":block-comment-decoy") */
dependencies {
    implementation(project(":real"))
}
"#;

        assert_eq!(extract_gradle_project_dependencies(content), vec![":real"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_uses_dialect_block_comment_nesting() {
        let groovy = r#"
/* Groovy closes this comment at the first terminator.
   /* project(":groovy-comment-decoy") */
dependencies { implementation(project(":real-groovy")) }
"#;
        assert_eq!(
            super::extract_gradle_project_dependencies(groovy, GradleDependencyDialect::Groovy),
            vec![":real-groovy"]
        );

        let kotlin = r#"
/* Kotlin keeps the outer comment open.
   /* project(":kotlin-nested-comment-decoy") */
   project(":kotlin-outer-comment-decoy")
*/
dependencies { implementation(project(":real-kotlin")) }
"#;
        assert_eq!(
            super::extract_gradle_project_dependencies(kotlin, GradleDependencyDialect::Kotlin),
            vec![":real-kotlin"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_uses_dialect_triple_quote_escapes() {
        let kotlin = r####"
val ordinary = "project(\":ordinary-string-decoy\")"
val raw = """project(":kotlin-raw-decoy") \"""
dependencies { implementation(project(":real-kotlin")) }
"####;
        let raw_start = kotlin.find("\"\"\"").unwrap();
        let raw_end = quoted_gradle_literal_end(
            kotlin.as_bytes(),
            raw_start,
            true,
            GradleDependencyDialect::Kotlin,
        )
        .unwrap();
        assert!(kotlin[raw_end..].starts_with("\ndependencies"));
        assert_eq!(
            super::extract_gradle_project_dependencies(kotlin, GradleDependencyDialect::Kotlin),
            vec![":real-kotlin"]
        );

        let groovy = r####"
def triple = """before \""" project(":groovy-triple-decoy") after"""
dependencies { implementation(project(":real-groovy")) }
"####;
        assert_eq!(
            super::extract_gradle_project_dependencies(groovy, GradleDependencyDialect::Groovy),
            vec![":real-groovy"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_ignores_grouped_non_dependency_calls() {
        let content = r#"
def quotient = (
    numerator
    / project(":grouped-division")
    / denominator
)
dependencies { implementation(project(":real")) }
"#;

        assert_eq!(extract_gradle_project_dependencies(content), vec![":real"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_skips_contextual_groovy_slashy_strings() {
        let content = r#"
return /project(":return-decoy")/
throw /project(":throw-decoy")/
switch (value) {
    case /project(":case-decoy")/: break
}
assert /project(":assert-decoy")/
yield /project(":yield-decoy")/
consume pattern: /project(":command-expression-decoy")/

def identifierQuotient = numerator / denominator
def numericQuotient = 12 / 3
dependencies {
    implementation(project(":real"))
}
"#;

        assert_eq!(extract_gradle_project_dependencies(content), vec![":real"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_rejects_qualified_calls() {
        let content = r#"
dependencies {
    implementation(receiver.project(":dot"))
    api(receiver?. /* trivia */ project(":safe-navigation"))
    runtimeOnly(receiver*. /* trivia */ project(":spread"))
    testImplementation(receiver.&project(":method-pointer"))
    compileOnly(receiver.@project(":direct-field"))
    implementation(project(":free"))
}
"#;

        assert_eq!(extract_gradle_project_dependencies(content), vec![":free"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_splits_top_level_arguments() {
        let content = r#"
dependencies {
    implementation(project(configuration = "default", path = ":late-kotlin"))
    api(project(configuration: 'default', path: ':late-groovy'))
    runtimeOnly(project(
        options = mapOf("nested" to listOf(1, 2), "array" to [3, 4]),
        action = { value -> consume(value, mapOf("inner" to listOf(5, 6))) },
        path = ":balanced:stacked",
    ))
    implementation(project(":first", path = ":duplicate"))
    implementation(project(path = ":one", path = ":two"))
    implementation(project(path = resolvePath(":computed")))
    implementation(project(configuration = "no-path"))
    implementation(project(configuration = "default", ":late-positional"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":late-kotlin", ":late-groovy", ":balanced:stacked"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_keeps_balanced_blank_line_continuations() {
        let content = r#"
dependencies {
    implementation(project(":real",

        selectConfiguration(project(":comma-decoy"))
    ))
    implementation(project(path = choosePath(":computed") +

        selectConfiguration(project(":operator-decoy"))
    ))
    implementation(project(":after-balanced-calls"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":real", ":after-balanced-calls"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_recovers_after_malformed_calls() {
        let content = r#"
dependencies {
    implementation(project(":unclosed"

    implementation(project(":after-unclosed"))
    implementation(project(path = ":mismatched"])

    implementation(project(":after-mismatch"))
    implementation(project(":another-unclosed"
        project(":nested-malformed-decoy")

    implementation(project(":after-nested-malformed"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![
                ":after-unclosed",
                ":after-mismatch",
                ":after-nested-malformed"
            ]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_quarantines_malformed_spans() {
        let content = r#"
dependencies {
    implementation(project(":bad"] + project(":nested-decoy"))

    implementation(project(":after-blank-line"))
    implementation(project(action = run(); ":bad"] + project(":pre-mismatch-boundary-decoy"))

    implementation(project(":after-pre-mismatch-boundary"))
    implementation(project(":unclosed"
    project(":same-indent-nested-decoy")

    implementation(project(":after-second-blank-line"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![
                ":after-blank-line",
                ":after-pre-mismatch-boundary",
                ":after-second-blank-line"
            ]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_keeps_nested_malformed_remainders_quarantined() {
        let content = r#"
dependencies {
    implementation(project(":bad-provider"] + provider({

        project(":nested-provider-decoy")
    }))

    implementation(project(":after-provider"))
    implementation(project(":bad-closure"] + ({

        project(":nested-closure-decoy")
    }))

    implementation(project(":after-closure"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":after-provider", ":after-closure"]
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
