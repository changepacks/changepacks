use regex::Regex;

/// Update version in build.gradle.kts content
///
/// # Panics
///
/// Panics if the internal regex pattern fails to compile. This should never
/// happen as the patterns are hardcoded.
#[must_use]
pub fn update_version_in_kts(content: &str, new_version: &str) -> String {
    // Pattern 1: version = "1.0.0"
    let simple_pattern = Regex::new(r#"(?m)^(version\s*=\s*)"[^"]+""#).unwrap();
    if simple_pattern.is_match(content) {
        return simple_pattern
            .replace(content, format!(r#"${{1}}"{new_version}""#))
            .to_string();
    }

    // Pattern 2: version = project.findProperty("...") ?: "1.0.0"
    let fallback_pattern =
        Regex::new(r#"(?m)^(version\s*=\s*project\.findProperty\([^)]+\)\s*\?:\s*)"[^"]+""#)
            .unwrap();
    if fallback_pattern.is_match(content) {
        return fallback_pattern
            .replace(content, format!(r#"${{1}}"{new_version}""#))
            .to_string();
    }

    content.to_string()
}

/// Update version in build.gradle (Groovy) content
///
/// # Panics
///
/// Panics if the internal regex pattern fails to compile. This should never
/// happen as the patterns are hardcoded.
#[must_use]
pub fn update_version_in_groovy(content: &str, new_version: &str) -> String {
    // Pattern 1: version = '1.0.0' or version = "1.0.0"
    let assign_pattern = Regex::new(r#"(?m)^(version\s*=\s*)['"][^'"]+['"]"#).unwrap();
    if assign_pattern.is_match(content) {
        return assign_pattern
            .replace(content, format!(r"${{1}}'{new_version}'"))
            .to_string();
    }

    // Pattern 2: version '1.0.0' or version "1.0.0"
    let space_pattern = Regex::new(r#"(?m)^(version\s+)['"][^'"]+['"]"#).unwrap();
    if space_pattern.is_match(content) {
        return space_pattern
            .replace(content, format!(r"${{1}}'{new_version}'"))
            .to_string();
    }

    content.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_version_in_kts_simple() {
        let content = r#"
plugins {
    id("java")
}

group = "com.example"
version = "1.0.0"
"#;
        let updated = update_version_in_kts(content, "1.0.1");
        assert!(updated.contains(r#"version = "1.0.1""#));
    }

    #[test]
    fn test_update_version_in_kts_with_fallback() {
        let content = r#"
group = "com.devfive"
version = project.findProperty("releaseVersion") ?: "1.0.11"
"#;
        let updated = update_version_in_kts(content, "1.0.12");
        assert!(
            updated.contains(r#"version = project.findProperty("releaseVersion") ?: "1.0.12""#)
        );
    }

    #[test]
    fn test_update_version_in_groovy_assign() {
        let content = r#"
group = 'com.example'
version = '2.0.0'
"#;
        let updated = update_version_in_groovy(content, "2.0.1");
        assert!(updated.contains("version = '2.0.1'"));
    }

    #[test]
    fn test_update_version_in_groovy_space() {
        let content = r#"
group = 'com.example'
version '3.0.0'
"#;
        let updated = update_version_in_groovy(content, "3.0.1");
        assert!(updated.contains("version '3.0.1'"));
    }
}
