//! Batched Gradle project metadata discovery.
//!
//! Runs the Gradle wrapper once per wrapper root with a generated init script
//! that prints one prefixed JSON record per evaluated project, then parses those
//! records back into typed values. `finder.rs` owns project discovery and only
//! consumes [`get_gradle_metadata`] plus the resulting
//! [`GradleWrapperMetadata`] / [`GradleProperties`] values.

use anyhow::{Context, Result};
use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use crate::finder::GradleCommandSpec;

macro_rules! gradle_metadata_prefix {
    () => {
        "__CHANGEPACKS_GRADLE_METADATA_V1__"
    };
}

pub(crate) const GRADLE_METADATA_PREFIX: &str = gradle_metadata_prefix!();

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

/// Project info obtained from batched Gradle metadata.
#[derive(Clone, Debug)]
pub(crate) struct GradleProperties {
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) has_subprojects: bool,
    pub(crate) has_publish_task: bool,
    pub(crate) has_publish_to_maven_local_task: bool,
}

#[derive(Debug)]
pub(crate) struct GradleMetadataRecord {
    pub(crate) project_path: String,
    pub(crate) properties: GradleProperties,
}

#[derive(Debug)]
pub(crate) struct GradleWrapperMetadata {
    pub(crate) by_project_dir: HashMap<PathBuf, GradleMetadataRecord>,
    pub(crate) project_names_by_path: HashMap<String, String>,
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

/// Trims a Gradle property and drops Gradle's `unspecified` sentinel.
///
/// The owned `String` is reused when it is already trimmed, which is the common
/// case, so only a value carrying surrounding whitespace pays for a new
/// allocation. An empty string is a legitimate value and is kept.
fn normalized_gradle_property(value: Option<String>) -> Option<String> {
    value.and_then(|mut value| {
        let trimmed = value.trim();
        if trimmed == "unspecified" {
            return None;
        }
        if trimmed.len() != value.len() {
            value = trimmed.to_owned();
        }
        Some(value)
    })
}

/// Parses one metadata record, returning its raw emitted directory alongside it.
///
/// The directory is only needed until [`get_gradle_metadata`] canonicalizes it
/// into the `by_project_dir` key, so it travels beside the record instead of
/// inside it and is dropped once the record is stored.
fn parse_gradle_metadata_record(json: &str) -> Result<(PathBuf, GradleMetadataRecord)> {
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

    Ok((
        PathBuf::from(project_dir),
        GradleMetadataRecord {
            project_path,
            properties: GradleProperties {
                name: normalized_gradle_property(Some(name)),
                version: normalized_gradle_property(version),
                has_subprojects,
                has_publish_task,
                has_publish_to_maven_local_task,
            },
        },
    ))
}

fn parse_gradle_metadata_records(output: &str) -> Result<Vec<(PathBuf, GradleMetadataRecord)>> {
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

pub(crate) async fn get_gradle_metadata(
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
    // Every record needs its directory canonicalized before the purely CPU-bound
    // duplicate detection below can run, and `tokio::fs::canonicalize` is a
    // blocking syscall dispatched to the blocking pool. Awaiting one per record
    // inside the merge loop serializes those syscalls across every subproject of
    // a multi-project build, so they are issued together here instead. The
    // results are then inspected in record order and the first failing one is
    // reported, keeping error selection tied to emission order rather than to
    // whichever syscall happened to fail first.
    let canonicalized = futures::future::join_all(
        records
            .iter()
            .map(|(raw_dir, _)| tokio::fs::canonicalize(raw_dir)),
    )
    .await;
    let mut normalized_dirs = Vec::with_capacity(canonicalized.len());
    for ((raw_dir, record), normalized_dir) in records.iter().zip(canonicalized) {
        normalized_dirs.push(normalized_dir.with_context(|| {
            format!(
                "Failed to normalize Gradle metadata directory '{}' for project '{}' emitted by '{}'",
                raw_dir.display(),
                record.project_path,
                gradlew.display()
            )
        })?);
    }

    let mut by_project_dir: HashMap<PathBuf, GradleMetadataRecord> =
        HashMap::with_capacity(records.len());
    let mut project_names_by_path = HashMap::with_capacity(records.len());
    for ((_raw_dir, record), normalized_dir) in records.into_iter().zip(normalized_dirs) {
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
        match project_names_by_path.entry(project_path) {
            Entry::Occupied(previous) => {
                return Err(anyhow::anyhow!(
                    "Duplicate Gradle metadata project path '{}' from '{}': projects '{}' and '{}'",
                    previous.key(),
                    gradlew.display(),
                    previous.get(),
                    project_name
                ));
            }
            Entry::Vacant(slot) => {
                slot.insert(project_name);
            }
        }
        match by_project_dir.entry(normalized_dir) {
            Entry::Occupied(previous) => {
                return Err(anyhow::anyhow!(
                    "Duplicate Gradle metadata records for normalized directory '{}' from '{}': projects '{}' and '{}'",
                    previous.key().display(),
                    gradlew.display(),
                    previous.get().project_path,
                    record.project_path
                ));
            }
            Entry::Vacant(slot) => {
                slot.insert(record);
            }
        }
    }

    Ok(GradleWrapperMetadata {
        by_project_dir,
        project_names_by_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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
        let (raw_dir, record) = &records[0];
        assert_eq!(*raw_dir, PathBuf::from(r"C:\repo with spaces\모듈"));
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

    /// Every semantic guard in `parse_gradle_metadata_record` is pinned by the
    /// tests above, but the syntactic guard that turns a `serde_json` failure
    /// into `invalid Gradle metadata JSON object` was not. Gradle stdout is the
    /// least controllable input this crate parses, so a prefixed line that is
    /// valid JSON but not an object, or is truncated mid-object, is a realistic
    /// failure whose wording callers see.
    #[rstest]
    #[case::json_array("[1,2]")]
    #[case::truncated_object(r#"{"projectDir":"/repo""#)]
    #[case::json_null("null")]
    fn test_parse_gradle_metadata_record_rejects_non_object_json(#[case] json: &str) {
        let error = parse_gradle_metadata_record(json).unwrap_err();
        let message = format!("{error:#}");

        assert!(
            message.contains("invalid Gradle metadata JSON object"),
            "{message}"
        );
    }

    #[test]
    fn test_parse_gradle_metadata_records_reports_non_object_json_with_line_index() {
        let output = concat!(
            "ordinary output\n",
            "__CHANGEPACKS_GRADLE_METADATA_V1__[1,2]\n",
        );

        let error = parse_gradle_metadata_records(output).unwrap_err();
        let message = format!("{error:#}");

        assert!(
            message.contains("malformed Gradle metadata record at line 2"),
            "{message}"
        );
        assert!(
            message.contains("invalid Gradle metadata JSON object"),
            "{message}"
        );
    }

    #[rstest]
    #[case(None, None)]
    #[case(Some("1.0"), Some("1.0"))]
    #[case(Some("  1.0  "), Some("1.0"))]
    #[case(Some("\t1.0\n"), Some("1.0"))]
    #[case(Some("unspecified"), None)]
    #[case(Some("  unspecified  "), None)]
    #[case(Some(""), Some(""))]
    #[case(Some("   "), Some(""))]
    #[case(Some("unspecified-core"), Some("unspecified-core"))]
    fn test_normalized_gradle_property(
        #[case] input: Option<&str>,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(
            normalized_gradle_property(input.map(str::to_owned)).as_deref(),
            expected
        );
    }

    #[test]
    fn test_normalized_gradle_property_reuses_already_trimmed_allocation() {
        let value = String::from("1.0.0");
        let original_ptr = value.as_ptr();

        let normalized = normalized_gradle_property(Some(value)).unwrap();

        assert_eq!(normalized, "1.0.0");
        assert_eq!(normalized.as_ptr(), original_ptr);
    }

    /// The guard rejects the call before any temporary file is created or any
    /// process is spawned, so this test stays portable and touches neither the
    /// filesystem nor a Gradle wrapper even though the paths below do not exist.
    #[tokio::test]
    async fn test_get_gradle_metadata_rejects_unavailable_java() {
        let error = get_gradle_metadata(Path::new("gradlew"), Path::new("."), false)
            .await
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("Java is required"), "{message}");
        assert!(message.contains("JAVA_HOME"), "{message}");
    }
}
