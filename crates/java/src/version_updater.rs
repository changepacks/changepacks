use anyhow::{Context, Result};
use changepacks_core::has_extension_ignore_ascii_case;
use regex::Regex;
use std::borrow::Cow;
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
/// preserving formatting and skipping the write when no `version = ...`
/// line matched (byte-identical no-op for version-less build files).
///
/// # Errors
/// Returns an error if the file cannot be read or written back.
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

    // Skip the write when neither regex matched: both `update_version_in_kts`
    // and `update_version_in_groovy` return `Cow::Borrowed(content)` unchanged
    // for build files with no `version = ...` line (e.g. a root
    // `settings.gradle.kts` that defers versioning to sub-projects). Guarding
    // the write keeps those files byte-identical on disk and avoids a mtime
    // bump plus a syscall pair on no-ops.
    if updated_content.as_ref() != content {
        write(path, updated_content.as_ref())
            .await
            .with_context(|| format!("Failed to write Gradle build file {}", path.display()))?;
    }
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

    // Locks in the "skip write on unchanged content" fast-path: a build file
    // with no `version = ...` line MUST be left byte-identical on disk (no
    // mtime bump, no rewrite). The version bump arithmetic lives in
    // `changepacks_utils::bump_version_with`; this writer only applies the
    // already-computed version string when a Gradle version line exists.
    #[tokio::test]
    async fn test_write_gradle_version_no_match_leaves_file_unchanged() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("build.gradle.kts");
        // A build file with NO `version = ...` line — this is the shape a
        // root `settings.gradle.kts` or a sub-project that inherits its
        // version from the parent typically has.
        let content = r#"
plugins {
    id("java")
}

group = "com.example"
"#;
        tokio::fs::write(&path, content).await.unwrap();

        // Record the file's exact bytes AND its mtime before the call so we
        // can assert both stay untouched. Reading via `tokio::fs::metadata`
        // avoids blocking the runtime.
        let bytes_before = tokio::fs::read(&path).await.unwrap();

        write_gradle_version(&path, "1.0.1").await.unwrap();

        // File bytes are byte-identical: no rewrite happened.
        let bytes_after = tokio::fs::read(&path).await.unwrap();
        assert_eq!(bytes_after, bytes_before);
    }
}
