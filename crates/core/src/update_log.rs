use std::{collections::HashMap, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::update_type::UpdateType;

#[derive(Debug, Serialize, Deserialize)]
pub struct ChangePackLog {
    changes: HashMap<PathBuf, UpdateType>,
    note: String,
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
