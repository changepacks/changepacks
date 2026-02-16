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
