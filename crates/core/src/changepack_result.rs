use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::update_type::UpdateType;

/// Single changepack log entry for aggregated results.
///
/// Contains the update type and note from a changepack log file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePackResultLog {
    /// Type of version update (Major, Minor, or Patch)
    r#type: UpdateType,
    /// User-provided changelog note
    note: String,
}

impl ChangePackResultLog {
    #[must_use]
    pub const fn new(r#type: UpdateType, note: String) -> Self {
        Self { r#type, note }
    }
}

/// Aggregated version update results for JSON output format.
///
/// Contains all changepack logs applied to a project, current version, next version,
/// and change status.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePackResult {
    /// All changepack logs applied to this project
    logs: Vec<ChangePackResultLog>,
    /// Current version before update
    version: Option<String>,
    /// New version after applying updates
    next_version: Option<String>,
    /// Project name from manifest
    name: Option<String>,
    /// Whether the project has uncommitted changes
    changed: bool,
    /// File path to the project manifest
    path: PathBuf,
}

impl ChangePackResult {
    #[must_use]
    pub const fn new(
        logs: Vec<ChangePackResultLog>,
        version: Option<String>,
        next_version: Option<String>,
        name: Option<String>,
        changed: bool,
        path: PathBuf,
    ) -> Self {
        Self {
            logs,
            version,
            next_version,
            name,
            changed,
            path,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn test_changepack_result_log_new() {
        let log = ChangePackResultLog::new(UpdateType::Minor, "Add new API endpoint".to_string());
        let debug_str = format!("{log:?}");

        assert!(debug_str.contains("ChangePackResultLog"));
        assert!(debug_str.contains("Minor"));
        assert!(debug_str.contains("Add new API endpoint"));
    }

    /// The `r#type` field is a raw identifier, so serde must emit the plain key `type`
    /// for every variant. `check` and `update --format json` publish that key, so a
    /// rename would silently break the documented JSON output contract.
    #[rstest]
    #[case(UpdateType::Major, "Major")]
    #[case(UpdateType::Minor, "Minor")]
    #[case(UpdateType::Patch, "Patch")]
    fn test_changepack_result_log_serialize(
        #[case] update_type: UpdateType,
        #[case] expected_type: &str,
    ) {
        let note = format!("Note for {expected_type}");
        let log = ChangePackResultLog::new(update_type, note.clone());
        let json: Value = serde_json::to_value(&log).unwrap();

        let object = json.as_object().unwrap();
        assert_eq!(
            object.get("type"),
            Some(&Value::String(expected_type.to_string()))
        );
        assert_eq!(object.get("note"), Some(&Value::String(note)));
        assert!(object.get("r#type").is_none());
        assert_eq!(object.len(), 2);
    }

    #[test]
    fn test_changepack_result_log_deserialize_from_json() {
        let source = json!({
            "type": "Major",
            "note": "Breaking API change",
        });

        let log: ChangePackResultLog = serde_json::from_value(source.clone()).unwrap();

        // The fields are private, so the round-trip through `to_value` is the only way
        // to assert that both keys were read back into the right places.
        assert_eq!(serde_json::to_value(&log).unwrap(), source);
    }

    #[test]
    fn test_changepack_result_new() {
        let logs = vec![ChangePackResultLog::new(
            UpdateType::Major,
            "Breaking changes".to_string(),
        )];
        let result = ChangePackResult::new(
            logs,
            Some("1.0.0".to_string()),
            Some("2.0.0".to_string()),
            Some("changepacks-core".to_string()),
            true,
            PathBuf::from("crates/core/Cargo.toml"),
        );
        let debug_str = format!("{result:?}");

        assert!(debug_str.contains("ChangePackResult"));
        assert!(debug_str.contains("1.0.0"));
        assert!(debug_str.contains("2.0.0"));
        assert!(debug_str.contains("changepacks-core"));
        assert!(debug_str.contains("changed: true"));
        assert!(debug_str.contains("crates/core/Cargo.toml"));
    }

    #[test]
    fn test_changepack_result_serialize_camel_case() {
        let logs = vec![ChangePackResultLog::new(
            UpdateType::Minor,
            "Add feature".to_string(),
        )];
        let result = ChangePackResult::new(
            logs,
            Some("1.1.0".to_string()),
            Some("1.2.0".to_string()),
            Some("core".to_string()),
            true,
            PathBuf::from("crates/core/Cargo.toml"),
        );
        let json: Value = serde_json::to_value(&result).unwrap();

        assert!(json.get("logs").is_some());
        assert!(json.get("version").is_some());
        assert!(json.get("nextVersion").is_some());
        assert!(json.get("name").is_some());
        assert!(json.get("changed").is_some());
        assert!(json.get("path").is_some());
        assert!(json.get("next_version").is_none());
    }

    #[test]
    fn test_changepack_result_deserialize_roundtrip() {
        let logs = vec![
            ChangePackResultLog::new(UpdateType::Major, "Breaking release".to_string()),
            ChangePackResultLog::new(UpdateType::Patch, "Hotfix".to_string()),
        ];
        let result = ChangePackResult::new(
            logs,
            Some("1.0.0".to_string()),
            Some("2.0.1".to_string()),
            Some("core".to_string()),
            false,
            PathBuf::from("crates/core/Cargo.toml"),
        );

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: ChangePackResult = serde_json::from_str(&json).unwrap();

        let original_value = serde_json::to_value(&result).unwrap();
        let deserialized_value = serde_json::to_value(&deserialized).unwrap();
        assert_eq!(deserialized_value, original_value);
    }

    #[test]
    fn test_changepack_result_with_empty_logs() {
        let result = ChangePackResult::new(
            Vec::new(),
            Some("1.0.0".to_string()),
            Some("1.0.1".to_string()),
            Some("core".to_string()),
            true,
            PathBuf::from("crates/core/Cargo.toml"),
        );
        let debug_str = format!("{result:?}");
        let json: Value = serde_json::to_value(&result).unwrap();

        assert!(debug_str.contains("logs: []"));
        assert!(json.get("logs").unwrap().as_array().unwrap().is_empty());
    }

    #[test]
    fn test_changepack_result_with_none_fields() {
        let logs = vec![ChangePackResultLog::new(
            UpdateType::Patch,
            "No version bump metadata".to_string(),
        )];
        let result = ChangePackResult::new(
            logs,
            None,
            None,
            None,
            false,
            PathBuf::from("crates/core/Cargo.toml"),
        );
        let json: Value = serde_json::to_value(&result).unwrap();

        assert!(json.get("version").unwrap().is_null());
        assert!(json.get("nextVersion").unwrap().is_null());
        assert!(json.get("name").unwrap().is_null());
        assert_eq!(json.get("changed"), Some(&Value::Bool(false)));
    }
}
