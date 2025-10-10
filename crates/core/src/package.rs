use crate::update_type::UpdateType;

#[derive(Debug)]
pub struct Package {
    name: String,
    version: String,
    path: String,
}

impl Package {
    pub fn new(name: String, version: String, path: String) -> Self {
        Self {
            name,
            version,
            path,
        }
    }

    /// Update the version of the package
    pub fn next_version(&mut self, update_type: UpdateType) -> String {
        let mut version_parts = self.version.split(".").collect::<Vec<&str>>();
        let version = match update_type {
            UpdateType::Major => 0,
            UpdateType::Minor => 1,
            UpdateType::Patch => 2,
        };
        let version_part = (version_parts[version].parse::<usize>().unwrap() + 1).to_string();
        version_parts[version] = version_part.as_str();
        version_parts.join(".")
    }
}
