use std::fs::canonicalize;

/// Find a file in the given root directory
pub fn find_root_path_by_files(root: &str, file_names: &[&str]) -> Option<String> {
    let dir_path = canonicalize(root).unwrap();
    let mut dir_path = dir_path.as_path();
    loop {
        if file_names
            .iter()
            .any(|file_name| dir_path.join(file_name).exists())
        {
            return Some(dir_path.to_string_lossy().to_string());
        } else if let Some(parent) = dir_path.parent() {
            dir_path = parent;
        } else {
            return None;
        }
    }
}
