use anyhow::{Context, Result, bail};
use changepacks_core::has_extension_ignore_ascii_case;
use regex::Regex;
use std::borrow::Cow;
use std::path::Path;
use std::sync::LazyLock;
use tokio::fs::{read_to_string, write};

static KTS_SIMPLE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(\s*version\s*=\s*)"[^"]+""#).expect("hardcoded regex must compile")
});

static KTS_FALLBACK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(\s*version\s*=\s*project\.findProperty\([^)]+\)\s*\?:\s*)"[^"]+""#)
        .expect("hardcoded regex must compile")
});

/// Try to replace content using a sequence of regex patterns.
/// Returns the first successful replacement (as `Cow::Owned`), or `Cow::Borrowed(content)` if none match.
fn replace_first_match<'a>(
    content: &'a str,
    patterns: &[&Regex],
    replacement: &str,
) -> Cow<'a, str> {
    for pattern in patterns {
        if let Cow::Owned(updated) = pattern.replace(content, replacement) {
            return Cow::Owned(updated);
        }
    }
    Cow::Borrowed(content)
}

/// Update version in build.gradle.kts content
#[must_use]
pub fn update_version_in_kts<'a>(content: &'a str, new_version: &str) -> Cow<'a, str> {
    let replacement = format!(r#"${{1}}"{new_version}""#);
    replace_first_match(
        content,
        &[&KTS_SIMPLE_PATTERN, &KTS_FALLBACK_PATTERN],
        &replacement,
    )
}

static GROOVY_ASSIGN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(\s*version\s*=\s*)(['"])[^'"]+['"]"#).expect("hardcoded regex must compile")
});

static GROOVY_SPACE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(\s*version\s+)(['"])[^'"]+['"]"#).expect("hardcoded regex must compile")
});

/// Update version in build.gradle (Groovy) content
#[must_use]
pub fn update_version_in_groovy<'a>(content: &'a str, new_version: &str) -> Cow<'a, str> {
    let replacement = format!(r"${{1}}${{2}}{new_version}${{2}}");
    replace_first_match(
        content,
        &[&GROOVY_ASSIGN_PATTERN, &GROOVY_SPACE_PATTERN],
        &replacement,
    )
}

/// Write `new_version` into a Gradle build file (`.kts` or Groovy),
/// preserving formatting.
///
/// # Errors
/// Returns an error if the file cannot be read or written, or if no supported
/// editable version declaration exists.
pub async fn write_gradle_version(path: &Path, new_version: &str) -> Result<()> {
    let content = read_to_string(path)
        .await
        .with_context(|| format!("Failed to read Gradle build file {}", path.display()))?;

    // `Path::extension()` already returns the trailing extension component,
    // so the previous `file_name().and_then(to_str) → Path::new(...).extension()`
    // trip through a fresh `Path` was redundant. Behaviour is preserved on
    // extension-less inputs: `Path::extension()` yields `None` when the file
    // stem is empty or missing, matching the old `unwrap_or_default() →
    // Path::new("").extension() == None` fallthrough.
    let is_kts = has_extension_ignore_ascii_case(path, "kts");

    let updated_content = if is_kts {
        update_version_in_kts(&content, new_version)
    } else {
        update_version_in_groovy(&content, new_version)
    };

    if updated_content.as_ref() == content {
        bail!(
            "No supported editable version declaration found in Gradle build file {}",
            path.display()
        );
    }

    write(path, updated_content.as_ref())
        .await
        .with_context(|| format!("Failed to write Gradle build file {}", path.display()))?;
    Ok(())
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
    fn test_update_version_in_kts_simple_preserves_space_indentation_byte_for_byte() {
        let content = "plugins {\r\n    version = \"1.0.0\" // keep this comment\r\n}\r\n";
        let updated = update_version_in_kts(content, "1.0.1");

        assert_eq!(
            updated,
            "plugins {\r\n    version = \"1.0.1\" // keep this comment\r\n}\r\n"
        );
    }

    #[test]
    fn test_update_version_in_kts_simple_preserves_tab_indentation_byte_for_byte() {
        let content = "plugins {\n\tversion\t=\t\"1.0.0\"\n}\n";
        let updated = update_version_in_kts(content, "1.0.1");

        assert_eq!(updated, "plugins {\n\tversion\t=\t\"1.0.1\"\n}\n");
    }

    #[test]
    fn test_update_version_in_kts_fallback_preserves_space_indentation_byte_for_byte() {
        let content = "allprojects {\r\n    version = project.findProperty(\"releaseVersion\") ?: \"1.0.11\" // fallback\r\n}\r\n";
        let updated = update_version_in_kts(content, "1.0.12");

        assert_eq!(
            updated,
            "allprojects {\r\n    version = project.findProperty(\"releaseVersion\") ?: \"1.0.12\" // fallback\r\n}\r\n"
        );
    }

    #[test]
    fn test_update_version_in_kts_fallback_preserves_tab_indentation_byte_for_byte() {
        let content = "allprojects {\n\tversion\t=\tproject.findProperty(\"releaseVersion\")\t?:\t\"1.0.11\"\n}\n";
        let updated = update_version_in_kts(content, "1.0.12");

        assert_eq!(
            updated,
            "allprojects {\n\tversion\t=\tproject.findProperty(\"releaseVersion\")\t?:\t\"1.0.12\"\n}\n"
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
    fn test_update_version_in_groovy_assign_preserves_double_quotes() {
        let content = r#"
group = 'com.example'
version = "2.0.0"
"#;
        let updated = update_version_in_groovy(content, "2.0.1");
        assert!(updated.contains(r#"version = "2.0.1""#));
    }

    #[test]
    fn test_update_version_in_groovy_space_preserves_double_quotes() {
        let content = r#"
group = 'com.example'
version "3.0.0"
"#;
        let updated = update_version_in_groovy(content, "3.0.1");
        assert!(updated.contains(r#"version "3.0.1""#));
    }

    #[test]
    fn test_update_version_in_groovy_assign_preserves_indentation() {
        let content = r#"
plugins {
    version = '1.0.0'
}
"#;
        let updated = update_version_in_groovy(content, "1.0.1");
        assert!(updated.contains("    version = '1.0.1'"));
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

    #[tokio::test]
    async fn test_write_gradle_version_errors_when_no_editable_version_exists() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("build.gradle.kts");
        let content = r#"
plugins {
    id("java")
}

group = "com.example"
"#;
        tokio::fs::write(&path, content).await.unwrap();
        let bytes_before = tokio::fs::read(&path).await.unwrap();

        let error = write_gradle_version(&path, "1.0.1").await.unwrap_err();

        let bytes_after = tokio::fs::read(&path).await.unwrap();
        assert!(
            error
                .to_string()
                .contains("No supported editable version declaration found")
        );
        assert!(error.to_string().contains(&path.display().to_string()));
        assert_eq!(bytes_after, bytes_before);
    }

    #[tokio::test]
    async fn test_write_gradle_version_updates_indented_kts_declaration() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("build.gradle.kts");
        let content = b"plugins {\r\n\tversion = \"1.0.0\" // preserve\r\n}\r\n";
        tokio::fs::write(&path, content).await.unwrap();

        write_gradle_version(&path, "1.0.1").await.unwrap();

        let updated = tokio::fs::read(&path).await.unwrap();
        assert_eq!(
            updated,
            b"plugins {\r\n\tversion = \"1.0.1\" // preserve\r\n}\r\n"
        );
    }
}
