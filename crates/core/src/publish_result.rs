use serde::Serialize;

#[derive(Serialize, Debug)]
pub struct PublishResult {
    result: bool,
    error: Option<String>,
}

impl PublishResult {
    pub fn new(result: bool, error: Option<String>) -> Self {
        Self { result, error }
    }
}
