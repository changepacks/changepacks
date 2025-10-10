#[derive(Debug)]
pub struct Workspace {
    path: String,
    version: Option<String>,
}

impl Workspace {
    pub fn new(path: String, version: Option<String>) -> Self {
        Self { path, version }
    }
}
