use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
#[cfg(test)]
use regex::Regex;
#[cfg(test)]
use std::sync::LazyLock;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::fs::read_to_string;
use tokio::process::Command;

use crate::{package::GradlePackage, workspace::GradleWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["build.gradle.kts", "build.gradle"];

const GRADLE_METADATA_PREFIX: &str = "__CHANGEPACKS_GRADLE_METADATA_V1__";

const GRADLE_METADATA_INIT_SCRIPT: &str = r#"import groovy.json.JsonOutput

gradle.projectsEvaluated { evaluatedGradle ->
    evaluatedGradle.rootProject.allprojects { project ->
        def record = [
            projectDir: project.projectDir.toPath().toAbsolutePath().normalize().toString(),
            projectPath: project.path,
            name: project.name,
            version: project.version == null ? null : project.version.toString(),
            aggregate: !project.childProjects.isEmpty()
        ]
        println("__CHANGEPACKS_GRADLE_METADATA_V1__" + JsonOutput.toJson(record))
    }
}
"#;

/// OS-specific Java executable filename, used by `which_java_in` and
/// `java_home_has_java` to avoid repeating the `cfg!(windows)` branch.
#[cfg(windows)]
const JAVA_EXECUTABLE: &str = "java.exe";
#[cfg(not(windows))]
const JAVA_EXECUTABLE: &str = "java";

/// Cached regexes for parsing gradlew `properties -q` output. `LazyLock`
/// mirrors the idiom already used in `crates/java/src/version_updater.rs`
/// (`KTS_SIMPLE_PATTERN` et al.) — the pattern strings are compile-time
/// constants, so re-compiling them on every `get_gradle_properties` call
/// (once per Gradle project per `check` / `update` / `publish`) was pure
/// per-call waste that this now avoids.
#[cfg(test)]
static NAME_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^name:\s*(.+)$").expect("hardcoded regex must compile"));

#[cfg(test)]
static VERSION_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^version:\s*(.+)$").expect("hardcoded regex must compile"));

#[cfg(test)]
static SUBPROJECTS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^subprojects:\s*(.+)$").expect("hardcoded regex must compile")
});

#[derive(Debug, Default)]
pub struct GradleProjectFinder {
    projects: HashMap<PathBuf, Project>,
    java_available: Option<bool>,
    metadata_by_wrapper: HashMap<PathBuf, HashMap<PathBuf, GradleMetadataRecord>>,
}

impl GradleProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Project info obtained from gradlew properties
#[derive(Clone, Debug, Default)]
struct GradleProperties {
    name: Option<String>,
    version: Option<String>,
    has_subprojects: bool,
}

#[derive(Clone, Debug)]
struct GradleMetadataRecord {
    project_dir: PathBuf,
    project_path: String,
    properties: GradleProperties,
}

#[derive(Debug)]
enum MetadataJsonValue {
    String(String),
    Bool(bool),
    Null,
}

struct MetadataJsonParser<'a> {
    input: &'a str,
    cursor: usize,
}

impl<'a> MetadataJsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, cursor: 0 }
    }

    fn parse_object(mut self) -> Result<HashMap<String, MetadataJsonValue>> {
        self.skip_whitespace();
        self.expect_char('{')?;
        self.skip_whitespace();

        let mut fields = HashMap::new();
        if self.consume_char('}') {
            self.ensure_finished()?;
            return Ok(fields);
        }

        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect_char(':')?;
            self.skip_whitespace();
            let value = self.parse_value()?;
            anyhow::ensure!(
                fields.insert(key.clone(), value).is_none(),
                "duplicate JSON field '{key}'"
            );
            self.skip_whitespace();
            if self.consume_char('}') {
                break;
            }
            self.expect_char(',')?;
        }

        self.ensure_finished()?;
        Ok(fields)
    }

    fn parse_value(&mut self) -> Result<MetadataJsonValue> {
        match self.peek_char() {
            Some('"') => self.parse_string().map(MetadataJsonValue::String),
            Some('t') => {
                self.expect_keyword("true")?;
                Ok(MetadataJsonValue::Bool(true))
            }
            Some('f') => {
                self.expect_keyword("false")?;
                Ok(MetadataJsonValue::Bool(false))
            }
            Some('n') => {
                self.expect_keyword("null")?;
                Ok(MetadataJsonValue::Null)
            }
            Some(character) => Err(anyhow::anyhow!(
                "unsupported JSON value starting with '{character}' at byte {}",
                self.cursor
            )),
            None => Err(anyhow::anyhow!("unexpected end of JSON value")),
        }
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect_char('"')?;
        let mut value = String::new();
        loop {
            let character = self
                .next_char()
                .context("unterminated JSON string in Gradle metadata")?;
            match character {
                '"' => return Ok(value),
                '\\' => self.parse_string_escape(&mut value)?,
                character if character <= '\u{1f}' => {
                    return Err(anyhow::anyhow!(
                        "unescaped control character in JSON string"
                    ));
                }
                character => value.push(character),
            }
        }
    }

    fn parse_string_escape(&mut self, value: &mut String) -> Result<()> {
        let escape = self
            .next_char()
            .context("unterminated JSON escape in Gradle metadata")?;
        match escape {
            '"' | '\\' | '/' => value.push(escape),
            'b' => value.push('\u{0008}'),
            'f' => value.push('\u{000c}'),
            'n' => value.push('\n'),
            'r' => value.push('\r'),
            't' => value.push('\t'),
            'u' => {
                let first = self.parse_unicode_escape()?;
                let code_point = if (0xd800..=0xdbff).contains(&first) {
                    self.expect_char('\\')?;
                    self.expect_char('u')?;
                    let second = self.parse_unicode_escape()?;
                    anyhow::ensure!(
                        (0xdc00..=0xdfff).contains(&second),
                        "invalid low surrogate in JSON string"
                    );
                    0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
                } else {
                    anyhow::ensure!(
                        !(0xdc00..=0xdfff).contains(&first),
                        "unexpected low surrogate in JSON string"
                    );
                    u32::from(first)
                };
                value.push(
                    char::from_u32(code_point)
                        .context("invalid Unicode code point in JSON string")?,
                );
            }
            escape => return Err(anyhow::anyhow!("invalid JSON escape '\\{escape}'")),
        }
        Ok(())
    }

    fn parse_unicode_escape(&mut self) -> Result<u16> {
        let start = self.cursor;
        let end = start
            .checked_add(4)
            .context("JSON Unicode escape offset overflow")?;
        let digits = self
            .input
            .get(start..end)
            .context("incomplete JSON Unicode escape")?;
        anyhow::ensure!(
            digits.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid JSON Unicode escape '{digits}'"
        );
        self.cursor = end;
        u16::from_str_radix(digits, 16).context("invalid JSON Unicode escape")
    }

    fn expect_keyword(&mut self, keyword: &str) -> Result<()> {
        anyhow::ensure!(
            self.input[self.cursor..].starts_with(keyword),
            "expected JSON keyword '{keyword}' at byte {}",
            self.cursor
        );
        self.cursor += keyword.len();
        Ok(())
    }

    fn ensure_finished(&mut self) -> Result<()> {
        self.skip_whitespace();
        anyhow::ensure!(
            self.cursor == self.input.len(),
            "unexpected trailing JSON content at byte {}",
            self.cursor
        );
        Ok(())
    }

    fn expect_char(&mut self, expected: char) -> Result<()> {
        let actual = self.next_char();
        anyhow::ensure!(
            actual == Some(expected),
            "expected '{expected}' at byte {}, found {actual:?}",
            self.cursor.saturating_sub(actual.map_or(0, char::len_utf8))
        );
        Ok(())
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.cursor += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek_char().is_some_and(char::is_whitespace) {
            let _ = self.next_char();
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.cursor..].chars().next()
    }

    fn next_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.cursor += character.len_utf8();
        Some(character)
    }
}

fn required_metadata_string(
    fields: &mut HashMap<String, MetadataJsonValue>,
    field: &str,
) -> Result<String> {
    match fields.remove(field) {
        Some(MetadataJsonValue::String(value)) => Ok(value),
        Some(value) => Err(anyhow::anyhow!(
            "Gradle metadata field '{field}' must be a string, got {value:?}"
        )),
        None => Err(anyhow::anyhow!(
            "Gradle metadata record is missing required field '{field}'"
        )),
    }
}

fn optional_metadata_string(
    fields: &mut HashMap<String, MetadataJsonValue>,
    field: &str,
) -> Result<Option<String>> {
    match fields.remove(field) {
        Some(MetadataJsonValue::String(value)) => Ok(Some(value)),
        Some(MetadataJsonValue::Null) => Ok(None),
        Some(value) => Err(anyhow::anyhow!(
            "Gradle metadata field '{field}' must be a string or null, got {value:?}"
        )),
        None => Err(anyhow::anyhow!(
            "Gradle metadata record is missing required field '{field}'"
        )),
    }
}

fn required_metadata_bool(
    fields: &mut HashMap<String, MetadataJsonValue>,
    field: &str,
) -> Result<bool> {
    match fields.remove(field) {
        Some(MetadataJsonValue::Bool(value)) => Ok(value),
        Some(value) => Err(anyhow::anyhow!(
            "Gradle metadata field '{field}' must be a boolean, got {value:?}"
        )),
        None => Err(anyhow::anyhow!(
            "Gradle metadata record is missing required field '{field}'"
        )),
    }
}

fn normalized_gradle_property(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| value != "unspecified")
}

fn parse_gradle_metadata_record(json: &str) -> Result<GradleMetadataRecord> {
    let mut fields = MetadataJsonParser::new(json).parse_object()?;
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

    Ok(GradleMetadataRecord {
        project_dir: PathBuf::from(project_dir),
        project_path,
        properties: GradleProperties {
            name: normalized_gradle_property(Some(name)),
            version: normalized_gradle_property(version),
            has_subprojects,
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
        // Async probe via the shared `changepacks_core::is_regular_file`
        // (a `tokio::fs::metadata().is_file()` check), matching the sibling
        // `java_home_has_java` and honoring the crate's no-blocking-I/O rule.
        if changepacks_core::is_regular_file(&candidate).await? {
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
    changepacks_core::is_regular_file(&candidate).await
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
    args.push(gradle_task_arg(project_dir, &gradlew_dir, task)?);
    args.extend_from_slice(additional_args);
    let output = GradleCommandSpec::new(&gradlew, &gradlew_dir, args)
        .command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("Failed to execute Gradle wrapper '{}'", gradlew.display()))?;

    Ok(changepacks_core::publish::PublishOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
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

#[cfg(test)]
fn gradle_property_value(caps: &regex::Captures) -> Option<String> {
    caps.get(1)
        .map(|m| m.as_str().trim())
        .filter(|v| *v != "unspecified")
        .map(std::string::ToString::to_string)
}

fn gradle_dependency_name(project_path: &str) -> Option<&str> {
    project_path
        .trim_matches(':')
        .rsplit(':')
        .next()
        .filter(|name| !name.is_empty())
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

    (candidate_count == 1)
        .then_some(project_path)
        .flatten()
        .and_then(gradle_dependency_name)
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
                        if !qualified
                            && let Some(dependency) =
                                gradle_dependency_from_arguments(content, &arguments, dialect)
                        {
                            dependencies.push(dependency);
                        }
                        cursor = end;
                        lexical.mark_byte(b')');
                    }
                    GradleCallScan::Malformed { resume } => {
                        cursor = resume.max(open + 1);
                        lexical = GradleLexState::default();
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
            lexical.mark_identifier(&bytes[cursor..end]);
            cursor = end;
            continue;
        }

        match bytes[cursor] {
            b'(' | b'[' => {
                continuation_group_depth += 1;
                lexical.mark_byte(bytes[cursor]);
            }
            b')' | b']' => {
                continuation_group_depth = continuation_group_depth.saturating_sub(1);
                lexical.mark_byte(bytes[cursor]);
            }
            b'\r' | b'\n' if continuation_group_depth == 0 => lexical.mark_statement_start(),
            byte if byte.is_ascii_whitespace() => {}
            byte => lexical.mark_byte(byte),
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
) -> Result<HashMap<PathBuf, GradleMetadataRecord>> {
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
    let output_result = command_spec
        .command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;
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
    let mut metadata = HashMap::with_capacity(records.len());
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
        if let Some(previous) = metadata.insert(normalized_dir.clone(), record) {
            return Err(anyhow::anyhow!(
                "Duplicate Gradle metadata records for normalized directory '{}' from '{}': projects '{}' and '{}'",
                normalized_dir.display(),
                gradlew.display(),
                previous.project_path,
                project_path
            ));
        }
    }

    Ok(metadata)
}

/// Get project properties using gradlew command.
///
/// Walks up the directory tree to find `gradlew`, then runs it with the correct
/// subproject path. For a subproject at `root/libs/core/`, this runs:
/// `./gradlew :libs:core:properties -q` from the root directory.
///
/// Returns `Err` when `gradlew` is not found or Java is not available.
///
#[cfg(test)]
async fn get_gradle_properties(
    project_dir: &Path,
    java_available: bool,
    max_depth: usize,
) -> Result<GradleProperties> {
    let (gradlew, gradlew_dir) = find_gradlew(project_dir, max_depth).await?.context(
        "Gradle wrapper (gradlew) not found. \
         Ensure the project root contains gradlew or gradlew.bat.",
    )?;

    // Gradle requires Java. Error early with a clear message rather than
    // letting gradlew produce a confusing "JAVA_HOME is not set" wall of text.
    // The availability probe (`java_is_available`) is async (it awaits
    // `is_regular_file`), so its result arrives here as the pre-computed
    // `java_available` local fed to `anyhow::ensure!`.
    anyhow::ensure!(
        java_available,
        "Java is required for Gradle projects but JAVA_HOME is not set and 'java' was not found on PATH.\n\
         Please set the JAVA_HOME environment variable or add java to your PATH."
    );

    let args = gradle_properties_args(project_dir, &gradlew_dir)?;
    let command_spec = GradleCommandSpec::new(&gradlew, &gradlew_dir, args);
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();
        return Err(anyhow::anyhow!(
            "Gradle properties failed for '{}' using '{}' with status {}{}",
            project_dir.display(),
            gradlew.display(),
            output.status,
            if stderr_trimmed.is_empty() {
                String::new()
            } else {
                format!("; stderr: {}", stderr_trimmed)
            }
        ));
    }

    Ok(parse_gradle_properties_output(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

#[cfg(test)]
fn parse_gradle_properties_output(output: &str) -> GradleProperties {
    let name = NAME_PATTERN
        .captures(output)
        .and_then(|caps| gradle_property_value(&caps));
    let version = VERSION_PATTERN
        .captures(output)
        .and_then(|caps| gradle_property_value(&caps));
    let has_subprojects = SUBPROJECTS_PATTERN
        .captures(output)
        .and_then(|caps| caps.get(1))
        .is_some_and(|value| value.as_str().trim() != "[]");

    GradleProperties {
        name,
        version,
        has_subprojects,
    }
}

#[cfg(test)]
fn gradle_properties_args(project_dir: &Path, gradlew_dir: &Path) -> Result<Vec<OsString>> {
    Ok(vec![
        gradle_task_arg(project_dir, gradlew_dir, "properties")?,
        OsString::from("-q"),
    ])
}

fn gradle_task_arg(project_dir: &Path, gradlew_dir: &Path, task: &str) -> Result<OsString> {
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
        let metadata = self
            .metadata_by_wrapper
            .get(&normalized_wrapper_dir)
            .and_then(|metadata| metadata.get(&normalized_project_dir))
            .cloned()
            .with_context(|| {
                format!(
                    "missing Gradle metadata record for project directory '{}' (normalized: '{}') from wrapper '{}'",
                    project_dir.display(),
                    normalized_project_dir.display(),
                    gradlew.display()
                )
            })?;
        let props = metadata.properties;

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
        let relative_path_key = relative_path.to_path_buf();
        let mut project = if is_workspace {
            Project::Workspace(Box::new(GradleWorkspace::new(
                name,
                version,
                path_key.clone(),
                relative_path_key,
            )))
        } else {
            Project::Package(Box::new(GradlePackage::new(
                name,
                version,
                path_key.clone(),
                relative_path_key,
            )))
        };

        for dependency in dependencies {
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
        fs::create_dir_all(&project_dir).unwrap();
        let manifest = project_dir.join(manifest_name);
        fs::write(&manifest, content).unwrap();
        create_mock_gradlew(&project_dir, MockGradlew::package("project", "1.0.0"));

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

    #[test]
    fn test_parse_gradle_properties_output_handles_values_and_unspecified() {
        let props = parse_gradle_properties_output(
            "name: demo\nversion: unspecified\nsubprojects: [project ':app']\n",
        );

        assert_eq!(props.name.as_deref(), Some("demo"));
        assert_eq!(props.version, None);
        assert!(props.has_subprojects);

        let empty =
            parse_gradle_properties_output("name: unspecified\nversion: 1.2.3\nsubprojects: []\n");
        assert_eq!(empty.name, None);
        assert_eq!(empty.version.as_deref(), Some("1.2.3"));
        assert!(!empty.has_subprojects);
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
        let record = format!(
            "{GRADLE_METADATA_PREFIX}{{\"projectDir\":{},\"projectPath\":\":\",\"name\":{},\"version\":{},\"aggregate\":{}}}",
            json_string(dir.to_string_lossy().as_ref()),
            json_string(mock.name),
            json_string(mock.version),
            mock.subprojects != "[]"
        );
        if cfg!(windows) {
            fs::write(
                dir.join("gradlew.bat"),
                format!(
                    "@echo off\r\n\
                     if \"%~1\"==\"-Dorg.gradle.configureondemand=false\" goto metadata\r\n\
                     echo name: {}\r\n\
                     echo version: {}\r\n\
                     echo subprojects: {}\r\n\
                     exit /b 0\r\n\
                     :metadata\r\n\
                     echo {record}\r\n",
                    mock.name, mock.version, mock.subprojects,
                ),
            )
            .unwrap();
        } else {
            let gradlew_path = dir.join("gradlew");
            fs::write(
                &gradlew_path,
                format!(
                    "#!/bin/sh\n\
                     if [ \"$1\" = '-Dorg.gradle.configureondemand=false' ]; then\n\
                       printf '%s\\n' '{record}'\n\
                     else\n\
                       printf '%s\\n' 'name: {}' 'version: {}' \"subprojects: {}\"\n\
                     fi\n",
                    mock.name, mock.version, mock.subprojects,
                ),
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
        emit_child_record: bool,
    ) -> PathBuf {
        let invocation_count = dir.join("wrapper-invocations.txt");
        let prefix = "__CHANGEPACKS_GRADLE_METADATA_V1__";
        let root_record = format!(
            "{prefix}{{\"projectDir\":{},\"projectPath\":\":\",\"name\":\"root project\",\"version\":\"1.2.3\",\"aggregate\":true}}",
            json_string(root_project_dir.to_string_lossy().as_ref())
        );
        let child_record = format!(
            "{prefix}{{\"projectDir\":{},\"projectPath\":\":module one\",\"name\":\"child project\",\"version\":\"2.3.4\",\"aggregate\":false}}",
            json_string(child_project_dir.to_string_lossy().as_ref())
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
                     if \"%~1\"==\"properties\" goto root\r\n\
                     if \"%~1\"==\":module one:properties\" goto child\r\n\
                     {batch_records}\
                     exit /b 0\r\n\
                     :root\r\n\
                     echo name: root project\r\n\
                     echo version: 1.2.3\r\n\
                     echo subprojects: [project ':module one']\r\n\
                     exit /b 0\r\n\
                     :child\r\n\
                     echo name: child project\r\n\
                     echo version: 2.3.4\r\n\
                     echo subprojects: []\r\n"
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
                     case \"$1\" in\n\
                       properties)\n\
                         printf '%s\\n' 'name: root project' 'version: 1.2.3' \"subprojects: [project ':module one']\"\n\
                         ;;\n\
                       ':module one:properties')\n\
                         printf '%s\\n' 'name: child project' 'version: 2.3.4' 'subprojects: []'\n\
                         ;;\n\
                       *)\n\
                         {unix_batch_records}\n\
                         ;;\n\
                     esac\n"
                ),
            )
            .unwrap();
            #[cfg(unix)]
            make_executable(&gradlew_path);
        }

        invocation_count
    }

    #[tokio::test]
    async fn test_gradle_metadata_command_disables_lazy_and_cached_configuration() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo");
        let child_dir = repo.join("child");
        fs::create_dir_all(&child_dir).unwrap();
        let root_manifest = repo.join("build.gradle.kts");
        fs::write(&root_manifest, "plugins { java }\n").unwrap();
        create_counting_multi_project_gradlew(&repo, &repo, &child_dir, true);

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
    fn test_parse_gradle_metadata_records_handles_spaces_unicode_and_unrelated_output() {
        let output = concat!(
            "Gradle configuration output\n",
            "unrelated __CHANGEPACKS_GRADLE_METADATA_V1__{not a record}\n",
            "__CHANGEPACKS_GRADLE_METADATA_V1__{\"projectDir\":\"C:\\\\repo with spaces\\\\모듈\",",
            "\"projectPath\":\":module one:유니코드\",\"name\":\"이름 with spaces\",",
            "\"version\":\"1.2.3-β\",\"aggregate\":false}\n",
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
            create_counting_multi_project_gradlew(&repo, &repo, &child_dir, true);

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
    async fn test_gradle_finder_errors_when_batch_metadata_record_is_missing() {
        let temp_dir = TempDir::new().unwrap();
        let repo = temp_dir.path().join("repo");
        let child_dir = repo.join("module one");
        fs::create_dir_all(&child_dir).unwrap();
        let root_manifest = repo.join("build.gradle.kts");
        let child_manifest = child_dir.join("build.gradle.kts");
        fs::write(&root_manifest, "plugins { java }\n").unwrap();
        fs::write(&child_manifest, "plugins { java }\n").unwrap();
        create_counting_multi_project_gradlew(&repo, &repo, &child_dir, false);

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
    fn test_gradle_properties_args_root_project() {
        let root = Path::new("repo");

        let args = gradle_properties_args(root, root).unwrap();

        assert_eq!(
            args,
            vec![OsString::from("properties"), OsString::from("-q")]
        );
    }

    #[test]
    fn test_gradle_properties_args_subproject() {
        let root = Path::new("repo");
        let subproject = root.join("libs").join("core");

        let args = gradle_properties_args(&subproject, root).unwrap();

        assert_eq!(
            args,
            vec![
                OsString::from(":libs:core:properties"),
                OsString::from("-q")
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
        let args = vec![OsString::from("properties"), OsString::from("-q")];

        let spec = GradleCommandSpec::new(&gradlew, Path::new("repo"), args);

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

        // Mock gradlew that reports subprojects (this is what makes it a workspace)
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

    #[tokio::test]
    async fn test_get_gradle_properties_no_gradlew() {
        let temp_dir = TempDir::new().unwrap();
        let subdir = temp_dir.path().join("isolated");
        fs::create_dir_all(&subdir).unwrap();
        // No gradlew in `subdir` or its parent. The walk is BOUNDED to
        // `max_depth`, so with depth 2 it scans only `subdir` and `temp_dir`
        // and cannot climb to a system gradlew higher up — so it reliably
        // returns Err ("Gradle wrapper (gradlew) not found").
        let result = get_gradle_properties(&subdir, true, 2).await;
        assert!(result.is_err());
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_with_mock() {
        let temp_dir = TempDir::new().unwrap();

        create_mock_gradlew(temp_dir.path(), MockGradlew::package("myproject", "1.2.3"));

        let props = get_gradle_properties(temp_dir.path(), true, 1)
            .await
            .unwrap();
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

        let props = get_gradle_properties(temp_dir.path(), true, 1)
            .await
            .unwrap();
        assert_eq!(props.name, Some("root".to_string()));
        assert!(props.has_subprojects);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_empty_subprojects() {
        let temp_dir = TempDir::new().unwrap();

        create_mock_gradlew(temp_dir.path(), MockGradlew::package("leaf", "1.0.0"));

        let props = get_gradle_properties(temp_dir.path(), true, 1)
            .await
            .unwrap();
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

        // Subproject `sub1` is one directory below the repo root → build file
        // `sub1/build.gradle.kts` (2 components) → `max_depth = 2`.
        let props = get_gradle_properties(&subproject, true, 2).await.unwrap();
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

        // Nested subproject `libs/core` is two directories below the repo root
        // → build file `libs/core/build.gradle.kts` (3 components) →
        // `max_depth = 3` (nesting levels + 1).
        let props = get_gradle_properties(&subproject, true, 3).await.unwrap();
        assert_eq!(props.name, Some("core".to_string()));
        assert_eq!(props.version, Some("3.1.0".to_string()));

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
            vec!["lib", "fixtures", "core", "cli", "shared"]
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

        assert_eq!(dependencies, vec!["공통", "인증", "cli"]);
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

        assert_eq!(dependencies, vec!["real"]);
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
            vec!["유니코드"]
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

        assert_eq!(extract_gradle_project_dependencies(content), vec!["real"]);
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
            vec!["real-groovy"]
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
            vec!["real-kotlin"]
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
            vec!["real-kotlin"]
        );

        let groovy = r####"
def triple = """before \""" project(":groovy-triple-decoy") after"""
dependencies { implementation(project(":real-groovy")) }
"####;
        assert_eq!(
            super::extract_gradle_project_dependencies(groovy, GradleDependencyDialect::Groovy),
            vec!["real-groovy"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_keeps_grouped_multiline_division_state() {
        let content = r#"
def quotient = (
    numerator
    / project(":grouped-division")
    / denominator
)
dependencies { implementation(project(":real")) }
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec!["grouped-division", "real"]
        );
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

        assert_eq!(extract_gradle_project_dependencies(content), vec!["real"]);
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

        assert_eq!(extract_gradle_project_dependencies(content), vec!["free"]);
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
            vec!["late-kotlin", "late-groovy", "stacked"]
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
            vec!["real", "after-balanced-calls"]
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
            vec!["after-unclosed", "after-mismatch", "after-nested-malformed"]
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
                "after-blank-line",
                "after-pre-mismatch-boundary",
                "after-second-blank-line"
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
            vec!["after-provider", "after-closure"]
        );
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
        fs::create_dir_all(&core_dir).unwrap();
        fs::create_dir_all(&app_dir).unwrap();

        let core_manifest = core_dir.join("build.gradle.kts");
        let app_manifest = app_dir.join("build.gradle.kts");
        fs::write(&core_manifest, "plugins { java }\n").unwrap();
        fs::write(
            &app_manifest,
            r#"dependencies {
    implementation(project(configuration = "default", path = ":modules:core"))
}
"#,
        )
        .unwrap();
        create_mock_gradlew(&core_dir, MockGradlew::package("core", "1.0.0"));
        create_mock_gradlew(&app_dir, MockGradlew::package("app", "1.0.0"));

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

        let props = get_gradle_properties(temp_dir.path(), true, 1)
            .await
            .unwrap();
        assert!(props.name.is_none());
        assert!(props.version.is_none());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_get_gradle_properties_gradlew_fails() {
        let temp_dir = TempDir::new().unwrap();

        create_failing_gradlew(temp_dir.path());

        let result = get_gradle_properties(temp_dir.path(), true, 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();

        // Error contains project path
        assert!(err_msg.contains(temp_dir.path().to_string_lossy().as_ref()));

        // Error contains exact wrapper path (platform-specific)
        let expected_wrapper = temp_dir.path().join(if cfg!(windows) {
            "gradlew.bat"
        } else {
            "gradlew"
        });
        assert!(
            err_msg.contains(expected_wrapper.display().to_string().as_str()),
            "Error should contain exact wrapper path: {}",
            expected_wrapper.display()
        );

        // Error contains exit status
        assert!(err_msg.contains("status"));

        // Error ends with trimmed stderr (proves trailing newline was removed)
        assert!(
            err_msg.ends_with("; stderr: broken build script"),
            "Error should end with trimmed stderr, got: {}",
            err_msg
        );

        temp_dir.close().unwrap();
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

        assert!(
            java_home_has_java(Some(temp_dir.path().as_os_str()))
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

        // visit should propagate the error from get_gradle_properties
        assert!(result.is_err());
        // No projects should be added when gradlew fails
        assert_eq!(finder.project_count(), 0);

        temp_dir.close().unwrap();
    }

    #[rstest]
    #[case(":lib", Some("lib"))]
    #[case(":a:b", Some("b"))]
    #[case(":::", None)]
    #[case("lib", Some("lib"))]
    #[case("", None)]
    #[case(":", None)]
    #[case("::", None)]
    #[case("a:b:c", Some("c"))]
    #[case(":a:b:c:d", Some("d"))]
    fn test_gradle_dependency_name(#[case] input: &str, #[case] expected: Option<&str>) {
        let result = gradle_dependency_name(input);
        assert_eq!(result, expected);
    }
}
