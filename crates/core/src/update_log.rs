use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::update_type::UpdateType;

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateLog {
    changes: HashMap<String, UpdateType>,
    notes: String,
    date: DateTime<Utc>,
}

impl UpdateLog {
    pub fn new(changes: HashMap<String, UpdateType>, notes: String) -> Self {
        Self {
            changes,
            notes,
            date: Utc::now(),
        }
    }

    pub fn changes(&self) -> &HashMap<String, UpdateType> {
        &self.changes
    }

    pub fn notes(&self) -> &str {
        &self.notes
    }
}
