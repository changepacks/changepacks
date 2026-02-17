use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::update_type::UpdateType;

/// Single changepack log entry for aggregated results.
///
/// Contains the update type and note from a changepack log file.
#[derive(Debug, Serialize, Deserialize)]
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

    use serde_json::Value;

    use super::*;

    #[test]
    fn test_changepack_result_log_new() {
        let log = ChangePackResultLog::new(UpdateType::Minor, "Add new API endpoint".to_string());
        let debug_str = format!("{:?}", log);

        assert!(debug_str.contains("ChangePackResultLog"));
        assert!(debug_str.contains("Minor"));
        assert!(debug_str.contains("Add new API endpoint"));
    }

    #[test]
    fn test_changepack_result_log_serialize() {
        let log = ChangePackResultLog::new(UpdateType::Patch, "Fix serialization bug".to_string());
        let json: Value = serde_json::to_value(&log).unwrap();

        assert_eq!(json.get("type"), Some(&Value::String("Patch".to_string())));
        assert_eq!(
            json.get("note"),
            Some(&Value::String("Fix serialization bug".to_string()))
        );
        assert!(json.get("r#type").is_none());
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
        let debug_str = format!("{:?}", result);

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
        let debug_str = format!("{:?}", result);
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
