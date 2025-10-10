#[derive(Debug)]
pub struct Workspace {
    path: String,
}

impl Workspace {
    pub fn new(path: String) -> Self {
        Self { path }
    }
}
