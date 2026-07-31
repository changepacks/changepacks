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

#[cfg(test)]
mod tests {
    use super::{GradleVersionScope, write_gradle_version};
    use changepacks_utils::test_support;

    /// The build-script write-back is the only place that attaches the
    /// `Failed to write Gradle build file <path>` context. Pin it so a
    /// permission failure stays attributable to the build script rather than
    /// surfacing as a bare `os error`.
    #[tokio::test]
    async fn test_write_gradle_version_build_file_write_error_names_context_and_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        std::fs::write(&build_path, "version = \"1.0.0\"\n").unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        test_support::set_readonly(&build_path, true);

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result =
            write_gradle_version(&build_path, "2.0.0", GradleVersionScope::ScriptOnly).await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        test_support::set_readonly(&build_path, false);

        let error = result.expect_err("write to a readonly Gradle build file must fail");
        let chain = format!("{error:#}");
        assert!(
            chain.contains(&format!(
                "Failed to write Gradle build file {}",
                build_path.display()
            )),
            "error chain should carry the build file write context, got: {chain}"
        );
    }

    /// The `gradle.properties` READ arm distinguishes "absent" (a legitimate
    /// `None`) from "unreadable" (a hard failure). Only a non-`NotFound` error
    /// reaches the context branch, so the fixture makes `gradle.properties` a
    /// DIRECTORY: reading it fails on every supported platform (`EISDIR` on
    /// Unix, access-denied on Windows) without depending on permission bits.
    ///
    /// The build script deliberately carries a perfectly editable declaration,
    /// pinning that an unreadable properties file aborts the whole update
    /// instead of silently falling through to the build-script write — the
    /// ambiguity checks cannot run without the properties content.
    #[tokio::test]
    async fn test_write_gradle_version_properties_read_error_names_context_and_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        let build_source = "version = \"1.0.0\"\n";
        std::fs::write(&build_path, build_source).unwrap();

        let properties_path = temp_dir.path().join("gradle.properties");
        std::fs::create_dir(&properties_path).unwrap();

        let error = write_gradle_version(&build_path, "2.0.0", GradleVersionScope::ScriptOnly)
            .await
            .expect_err("an unreadable gradle.properties must not be treated as absent");

        let chain = format!("{error:#}");
        assert!(
            chain.contains(&format!(
                "Failed to read Gradle properties file {}",
                properties_path.display()
            )),
            "error chain should carry the properties read context and path, got: {chain}"
        );
        assert!(
            error
                .chain()
                .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
            "failure must originate from the read itself, got: {chain}"
        );
        assert_eq!(
            std::fs::read_to_string(&build_path).unwrap(),
            build_source,
            "the build file must stay untouched when the properties read fails"
        );
    }

    /// The `gradle.properties` WRITE arm is only reached when the build script
    /// declares no editable version, so the fixture keeps the script version
    /// free and lets the properties file own the literal. A readonly properties
    /// file then fails the write, which must stay attributable to the
    /// properties file rather than surfacing as a bare `os error`.
    #[tokio::test]
    async fn test_write_gradle_version_properties_write_error_names_context_and_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        std::fs::write(&build_path, "plugins {\n    id(\"java\")\n}\n").unwrap();

        let properties_path = temp_dir.path().join("gradle.properties");
        let properties_source = b"group=com.example\nversion=1.0.0\n";
        std::fs::write(&properties_path, properties_source).unwrap();

        // The read succeeds (readonly still permits reads); it is the
        // write-back that must fail, so flip the readonly bit after seeding.
        test_support::set_readonly(&properties_path, true);

        // A NEW version guarantees the write is actually attempted against the
        // readonly file rather than being short-circuited as an unchanged no-op.
        let result =
            write_gradle_version(&build_path, "2.0.0", GradleVersionScope::ScriptOnly).await;

        // Restore write permission BEFORE asserting so `TempDir` cleanup
        // succeeds even if an assertion panics.
        test_support::set_readonly(&properties_path, false);

        let error = result.expect_err("write to a readonly gradle.properties must fail");
        let chain = format!("{error:#}");
        assert!(
            chain.contains(&format!(
                "Failed to write Gradle properties file {}",
                properties_path.display()
            )),
            "error chain should carry the properties write context and path, got: {chain}"
        );
        assert!(
            error
                .chain()
                .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
            "failure must originate from the write itself, got: {chain}"
        );
        assert_eq!(
            std::fs::read(&properties_path).unwrap(),
            properties_source,
            "a properties file that could not be written must stay byte-identical"
        );
    }
}
