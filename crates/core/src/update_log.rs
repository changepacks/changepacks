use std::{collections::HashMap, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::update_type::UpdateType;

/// On-disk changepack log entry with changes map, note, and timestamp.
///
/// Stored in `.changepacks/changepack_log_*.json` files and used to calculate
/// version updates during the update command.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChangePackLog {
    /// Map of package file paths to their update types
    changes: HashMap<PathBuf, UpdateType>,
    /// User-provided changelog note for this changepack
    note: String,
    /// UTC timestamp when this changepack was created
    date: DateTime<Utc>,
}

impl ChangePackLog {
    #[must_use]
    pub fn new(changes: HashMap<PathBuf, UpdateType>, note: String) -> Self {
        Self {
            changes,
            note,
            date: Utc::now(),
        }
    }

    #[must_use]
    pub fn changes(&self) -> &HashMap<PathBuf, UpdateType> {
        &self.changes
    }

    #[must_use]
    pub fn note(&self) -> &str {
        &self.note
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use chrono::{DateTime, Utc};

    use super::*;

    #[test]
    fn test_changepack_log_new() {
        let mut changes = HashMap::new();
        changes.insert(
            PathBuf::from("packages/foo/package.json"),
            UpdateType::Minor,
        );
        let note = "Add feature X".to_string();

        let log = ChangePackLog::new(changes.clone(), note.clone());

        assert_eq!(log.changes(), &changes);
        assert_eq!(log.note(), note);
    }

    #[test]
    fn test_changepack_log_changes_accessor() {
        let mut changes = HashMap::new();
        changes.insert(
            PathBuf::from("packages/foo/package.json"),
            UpdateType::Major,
        );
        changes.insert(PathBuf::from("crates/core/Cargo.toml"), UpdateType::Patch);

        let log = ChangePackLog::new(changes.clone(), "Update changes".to_string());

        assert_eq!(log.changes().len(), 2);
        assert_eq!(log.changes(), &changes);
        assert_eq!(
            log.changes()
                .get(&PathBuf::from("packages/foo/package.json")),
            Some(&UpdateType::Major)
        );
        assert_eq!(
            log.changes().get(&PathBuf::from("crates/core/Cargo.toml")),
            Some(&UpdateType::Patch)
        );
    }

    #[test]
    fn test_changepack_log_note_accessor() {
        let mut changes = HashMap::new();
        changes.insert(
            PathBuf::from("packages/foo/package.json"),
            UpdateType::Minor,
        );

        let log = ChangePackLog::new(changes, "Detailed changelog note".to_string());

        assert_eq!(log.note(), "Detailed changelog note");
    }

    #[test]
    fn test_changepack_log_empty_changes() {
        let log = ChangePackLog::new(HashMap::new(), "No package updates".to_string());

        assert!(log.changes().is_empty());
        assert_eq!(log.note(), "No package updates");
    }

    #[test]
    fn test_changepack_log_serialize_deserialize_roundtrip() {
        let mut changes = HashMap::new();
        changes.insert(
            PathBuf::from("packages/foo/package.json"),
            UpdateType::Minor,
        );
        changes.insert(PathBuf::from("crates/core/Cargo.toml"), UpdateType::Patch);
        let log = ChangePackLog::new(changes, "Roundtrip changelog note".to_string());

        let json = serde_json::to_string(&log).unwrap();
        let deserialized: ChangePackLog = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.changes(), log.changes());
        assert_eq!(deserialized.note(), log.note());
        assert_eq!(deserialized.date, log.date);
    }

    #[test]
    fn test_changepack_log_deserialize_from_json() {
        let json = r#"{
            "changes": {
                "packages/foo/package.json": "Minor",
                "crates/core/Cargo.toml": "Patch"
            },
            "note": "Ship feature and fix",
            "date": "2025-12-19T10:27:00.000Z"
        }"#;

        let log: ChangePackLog = serde_json::from_str(json).unwrap();
        let expected_date = DateTime::parse_from_rfc3339("2025-12-19T10:27:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(log.changes().len(), 2);
        assert_eq!(
            log.changes()
                .get(&PathBuf::from("packages/foo/package.json")),
            Some(&UpdateType::Minor)
        );
        assert_eq!(
            log.changes().get(&PathBuf::from("crates/core/Cargo.toml")),
            Some(&UpdateType::Patch)
        );
        assert_eq!(log.note(), "Ship feature and fix");
        assert_eq!(log.date, expected_date);
    }
}
