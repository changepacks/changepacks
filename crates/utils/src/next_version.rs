use core::update_type::UpdateType;

pub fn next_version(version: &str, update_type: UpdateType) -> String {
    let mut version_parts = version.split(".").collect::<Vec<&str>>();
    let version_index = match update_type {
        UpdateType::Major => 0,
        UpdateType::Minor => 1,
        UpdateType::Patch => 2,
    };
    let version_part = (version_parts[version_index].parse::<usize>().unwrap() + 1).to_string();
    version_parts[version_index] = version_part.as_str();
    version_parts.join(".")
}
