use std::ffi::OsStr;
use std::path::{Component, Path};

use anyhow::{Context, Result};

/// Check if a path contains `.changepacks` as a full path component.
///
/// Returns `true` if the path traverses a `.changepacks` directory component.
/// The check matches on a full path component so sibling names like
/// `.changepacks-backup/` or `notes-about-.changepacks.md` are NOT matched.
/// This is used to filter out changepack logs which are not user changes.
#[must_use]
pub fn contains_changepacks_component(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, Component::Normal(name) if name == OsStr::new(".changepacks")))
}

/// Whether a filesystem event on `candidate` should mark the project rooted at
/// `project_manifest` (its `package.json` / `Cargo.toml` / etc.) as changed.
///
/// Returns `false` for paths that traverse a `.changepacks` directory
/// component (changepack logs are not user changes) and paths that fall
/// outside the project's directory. The `.changepacks` check matches on a
/// full path component so sibling names like `.changepacks-backup/` or
/// `notes-about-.changepacks.md` are NOT swallowed. This is the
/// byte-identical logic used by both `Package::check_changed` and
/// `Workspace::check_changed` defaults; extracted here so a future fix stays
/// applied to both trait defaults in one place.
///
/// # Errors
/// Returns error if `project_manifest` has no parent directory.
pub(crate) fn should_mark_changed(candidate: &Path, project_manifest: &Path) -> Result<bool> {
    if contains_changepacks_component(candidate) {
        return Ok(false);
    }
    let project_dir = project_manifest.parent().context("Parent not found")?;
    Ok(candidate.starts_with(project_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // Path with `.changepacks` as a full component returns true
    #[case(".changepacks/changepack_log_x.json", true)]
    #[case("a/.changepacks/b", true)]
    #[case("/project/.changepacks/change.json", true)]
    // Path with similar name but different component returns false
    #[case(".changepacks-backup/file.json", false)]
    #[case("notes-about-.changepacks.md", false)]
    // Regular paths return false
    #[case("src/index.js", false)]
    #[case("packages/core/package.json", false)]
    fn test_contains_changepacks_component(#[case] path: &str, #[case] expected: bool) {
        assert_eq!(contains_changepacks_component(Path::new(path)), expected);
    }

    #[rstest]
    // A source file inside the project directory marks it changed.
    #[case("/project/src/index.js", "/project/package.json", true)]
    // Anything under the project's `.changepacks/` dir is a changepack log,
    // not a user change, so it must NOT mark the project changed.
    #[case("/project/.changepacks/change.json", "/project/package.json", false)]
    // A file belonging to a different project must not mark this one changed.
    #[case("/other-project/src/index.js", "/project/package.json", false)]
    // Sibling dirs/files whose name only *contains* ".changepacks" as a
    // substring are real user data and MUST still mark the project changed —
    // the guard matches on a full path component, not a substring. The old
    // substring check silently dropped these legitimate source edits.
    #[case(
        "/project/.changepacks-backup/pinned.json",
        "/project/package.json",
        true
    )]
    #[case("/project/notes-about-.changepacks.md", "/project/package.json", true)]
    fn test_should_mark_changed(
        #[case] candidate: &str,
        #[case] manifest: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(
            should_mark_changed(Path::new(candidate), Path::new(manifest)).unwrap(),
            expected
        );
    }

    #[test]
    fn test_should_mark_changed_errors_when_manifest_has_no_parent() {
        let candidate = Path::new("src/index.js");
        let manifest = Path::new("");
        let err = should_mark_changed(candidate, manifest).unwrap_err();
        assert!(err.to_string().contains("Parent not found"));
    }
}
