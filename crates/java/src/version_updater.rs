use anyhow::Result;
use changepacks_core::UpdateType;
use changepacks_utils::next_version;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;
use tokio::fs::{read_to_string, write};

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

/// Read a Gradle build file, compute its next version, apply the right
/// (`.kts` vs Groovy) rewrite in memory, write it back, and return the new
/// version string. Shared by both `GradlePackage::update_version` and
/// `GradleWorkspace::update_version` so a future change (extra file layouts,
/// error handling for malformed inputs) lives in exactly one place.
///
/// # Errors
/// Returns an error if the file cannot be read, the version cannot be
/// incremented, or the file cannot be written back.
pub async fn update_gradle_version_at(
    path: &Path,
    current_version: &str,
    update_type: UpdateType,
) -> Result<String> {
    let new_version = next_version(current_version, update_type)?;

    let content = read_to_string(path).await?;
    // `Path::extension()` already returns the trailing extension component,
    // so the previous `file_name().and_then(to_str) → Path::new(...).extension()`
    // trip through a fresh `Path` was redundant. Behaviour is preserved on
    // extension-less inputs: `Path::extension()` yields `None` when the file
    // stem is empty or missing, matching the old `unwrap_or_default() →
    // Path::new("").extension() == None` fallthrough.
    let is_kts = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("kts"));

    let updated_content = if is_kts {
        update_version_in_kts(&content, &new_version)
    } else {
        update_version_in_groovy(&content, &new_version)
    };

    write(path, updated_content).await?;
    Ok(new_version)
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

    #[test]
    fn test_update_version_in_kts_no_match() {
        let content = r#"
plugins {
    id("java")
}

group = "com.example"
"#;
        let result = update_version_in_kts(content, "2.0.0");
        assert_eq!(result, content);
    }

    #[test]
    fn test_update_version_in_groovy_no_match() {
        let content = r#"
plugins {
    id 'java'
}

group = 'com.example'
"#;
        let result = update_version_in_groovy(content, "2.0.0");
        assert_eq!(result, content);
    }
}
