use crate::properties_version::{PropertyAssignment, property_assignments};
use crate::version_lexer::{GradleDialect, candidate_ranges};
use anyhow::{Context, Result, bail};
use changepacks_core::has_extension_ignore_ascii_case;
#[cfg(test)]
use std::borrow::Cow;
use std::io::ErrorKind;
use std::ops::Range;
use std::path::Path;
use tokio::fs::{read, read_to_string, write};

/// Select which Gradle scopes may own the project version declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GradleVersionScope {
    /// Only a declaration in the build script's outermost scope.
    ScriptOnly,
    /// An outermost declaration or a direct declaration in a top-level
    /// `allprojects { ... }` block.
    ScriptAndAllProjects,
}

/// Replace the byte range `candidate` of `content` with `new_version`.
fn splice_version(content: &str, candidate: &Range<usize>, new_version: &str) -> String {
    let mut updated = String::with_capacity(content.len() - candidate.len() + new_version.len());
    updated.push_str(&content[..candidate.start]);
    updated.push_str(new_version);
    updated.push_str(&content[candidate.end..]);
    updated
}

/// Test-only: the production path splices the single candidate directly via
/// [`splice_version`]; this wrapper adds the empty/ambiguous arbitration that
/// only the isolated build-script helpers below need.
#[cfg(test)]
fn replace_candidate<'a>(
    content: &'a str,
    new_version: &str,
    candidates: Vec<Range<usize>>,
) -> Result<Cow<'a, str>> {
    match candidates.as_slice() {
        [] => bail!("No supported editable version declaration found"),
        [candidate] => Ok(Cow::Owned(splice_version(content, candidate, new_version))),
        candidates => bail!(
            "Ambiguous supported editable version declarations found ({} candidates)",
            candidates.len()
        ),
    }
}

/// Update version in build.gradle.kts content
///
/// Test-only: production version writing goes through [`write_gradle_version`],
/// which owns the build-script/`gradle.properties` arbitration. This helper
/// only exercises the build-script replacement half in isolation.
///
/// # Errors
/// Returns an error unless exactly one declaration exists in a supported scope.
#[cfg(test)]
pub(crate) fn update_version_in_kts<'a>(
    content: &'a str,
    new_version: &str,
    policy: GradleVersionScope,
) -> Result<Cow<'a, str>> {
    replace_candidate(
        content,
        new_version,
        candidate_ranges(content, policy, GradleDialect::Kotlin).editable,
    )
}

/// Update version in build.gradle (Groovy) content
///
/// Test-only: production version writing goes through [`write_gradle_version`],
/// which owns the build-script/`gradle.properties` arbitration. This helper
/// only exercises the build-script replacement half in isolation.
///
/// # Errors
/// Returns an error unless exactly one declaration exists in a supported scope.
#[cfg(test)]
pub(crate) fn update_version_in_groovy<'a>(
    content: &'a str,
    new_version: &str,
    policy: GradleVersionScope,
) -> Result<Cow<'a, str>> {
    replace_candidate(
        content,
        new_version,
        candidate_ranges(content, policy, GradleDialect::Groovy).editable,
    )
}

/// Write `new_version` into a Gradle build file (`.kts` or Groovy),
/// preserving formatting.
///
/// # Errors
/// Returns an error if the file cannot be read or written, or unless exactly
/// one editable version declaration exists in a supported scope.
pub async fn write_gradle_version(
    path: &Path,
    new_version: &str,
    policy: GradleVersionScope,
) -> Result<()> {
    let content = read_to_string(path)
        .await
        .with_context(|| format!("Failed to read Gradle build file {}", path.display()))?;

    let is_kts = has_extension_ignore_ascii_case(path, "kts");
    let script_candidates = if is_kts {
        candidate_ranges(&content, policy, GradleDialect::Kotlin)
    } else {
        candidate_ranges(&content, policy, GradleDialect::Groovy)
    };
    let properties_path = path.with_file_name("gradle.properties");
    let properties_content = match read(&properties_path).await {
        Ok(content) => Some(content),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "Failed to read Gradle properties file {}",
                    properties_path.display()
                )
            });
        }
    };
    let property_assignments = properties_content
        .as_deref()
        .map(property_assignments)
        .unwrap_or_default();

    if script_candidates.editable.len() > 1 {
        bail!(
            "Ambiguous supported editable version declarations found ({} candidates) in Gradle build file {}",
            script_candidates.editable.len(),
            path.display()
        );
    }
    if property_assignments.len() > 1 {
        bail!(
            "Ambiguous active version assignments found ({} candidates) in Gradle properties file {}",
            property_assignments.len(),
            properties_path.display()
        );
    }
    if matches!(
        property_assignments.as_slice(),
        [PropertyAssignment::Unsupported]
    ) {
        bail!(
            "The active version assignment is computed, continued, or otherwise non-literal in Gradle properties file {}",
            properties_path.display()
        );
    }
    if !script_candidates.editable.is_empty() && !property_assignments.is_empty() {
        bail!(
            "Ambiguous editable version sources found in both Gradle build file {} and Gradle properties file {}",
            path.display(),
            properties_path.display()
        );
    }

    if let [candidate] = script_candidates.editable.as_slice() {
        let updated_content = splice_version(&content, candidate, new_version);

        write(path, &updated_content)
            .await
            .with_context(|| format!("Failed to write Gradle build file {}", path.display()))?;
        return Ok(());
    }
    if script_candidates.has_unsupported {
        bail!(
            "The Gradle version source is computed or provider-backed in Gradle build file {}",
            path.display()
        );
    }
    if let (Some(properties_content), [PropertyAssignment::Literal(candidate)]) = (
        properties_content.as_deref(),
        property_assignments.as_slice(),
    ) {
        let mut updated =
            Vec::with_capacity(properties_content.len() - candidate.len() + new_version.len());
        updated.extend_from_slice(&properties_content[..candidate.start]);
        updated.extend_from_slice(new_version.as_bytes());
        updated.extend_from_slice(&properties_content[candidate.end..]);

        write(&properties_path, updated).await.with_context(|| {
            format!(
                "Failed to write Gradle properties file {}",
                properties_path.display()
            )
        })?;
        return Ok(());
    }

    bail!(
        "No supported editable version declaration found in Gradle build file {} or Gradle properties file {}",
        path.display(),
        properties_path.display()
    )
}
