use regex::Regex;
use std::sync::LazyLock;

static KTS_SIMPLE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(version\s*=\s*)"[^"]+""#).expect("hardcoded regex must compile")
});

static KTS_FALLBACK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(version\s*=\s*project\.findProperty\([^)]+\)\s*\?:\s*)"[^"]+""#)
        .expect("hardcoded regex must compile")
});

/// Update version in build.gradle.kts content
#[must_use]
pub fn update_version_in_kts(content: &str, new_version: &str) -> String {
    // Pattern 1: version = "1.0.0"
    if KTS_SIMPLE_PATTERN.is_match(content) {
        return KTS_SIMPLE_PATTERN
            .replace(content, format!(r#"${{1}}"{new_version}""#))
            .to_string();
    }

    // Pattern 2: version = project.findProperty("...") ?: "1.0.0"
    if KTS_FALLBACK_PATTERN.is_match(content) {
        return KTS_FALLBACK_PATTERN
            .replace(content, format!(r#"${{1}}"{new_version}""#))
            .to_string();
    }

    content.to_string()
}

static GROOVY_ASSIGN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(version\s*=\s*)['"][^'"]+['"]"#).expect("hardcoded regex must compile")
});

static GROOVY_SPACE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(version\s+)['"][^'"]+['"]"#).expect("hardcoded regex must compile")
});

/// Update version in build.gradle (Groovy) content
#[must_use]
pub fn update_version_in_groovy(content: &str, new_version: &str) -> String {
    // Pattern 1: version = '1.0.0' or version = "1.0.0"
    if GROOVY_ASSIGN_PATTERN.is_match(content) {
        return GROOVY_ASSIGN_PATTERN
            .replace(content, format!(r"${{1}}'{new_version}'"))
            .to_string();
    }

    // Pattern 2: version '1.0.0' or version "1.0.0"
    if GROOVY_SPACE_PATTERN.is_match(content) {
        return GROOVY_SPACE_PATTERN
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
