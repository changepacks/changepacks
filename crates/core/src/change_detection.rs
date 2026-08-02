use std::ffi::OsStr;
use std::path::{Component, Path};

use anyhow::{Context, Result};

/// The directory name that holds changepack logs, matched as a full path
/// component by [`contains_changepacks_component`].
const CHANGEPACKS_DIR: &str = ".changepacks";

/// Check if a path contains `.changepacks` as a full path component.
///
/// Returns `true` if the path traverses a `.changepacks` directory component.
/// The check matches on a full path component so sibling names like
/// `.changepacks-backup/` or `notes-about-.changepacks.md` are NOT matched.
/// This is used to filter out changepack logs which are not user changes.
#[must_use]
pub fn contains_changepacks_component(path: &Path) -> bool {
    // Fast reject before the component walk. A path that traverses a
    // `.changepacks` component necessarily contains the literal
    // `.changepacks` somewhere in its bytes, so a UTF-8 path that lacks that
    // substring cannot possibly match and never needs decoding component by
    // component. `str::contains` on a literal needle is memchr-accelerated,
    // which matters because this is the hottest predicate in the tool: it runs
    // once per (project, changed path) pair plus once per diff and worktree
    // status entry.
    //
    // This branch can only ever answer `false`, never `true`, so it cannot
    // widen the match: substring decoys such as `.changepacks-backup/file.json`
    // and `notes-about-.changepacks.md` still fall through to the exact
    // component comparison below and are still rejected there. A non-UTF-8
    // path makes `to_str` return `None` and likewise falls through unchanged.
    if let Some(text) = path.to_str()
        && !text.contains(CHANGEPACKS_DIR)
    {
        return false;
    }
    path.components()
        .any(|c| matches!(c, Component::Normal(name) if name == OsStr::new(CHANGEPACKS_DIR)))
}

/// Resolve the directory that contains `manifest_path`.
///
/// This is the single owner of the `"Parent not found - {}"` context message.
/// That exact text is a **pinned contract**: both
/// [`should_mark_changed`] and `changepacks_utils::is_workspace_by_sibling`
/// route their `Path::parent` resolution through here, and both crates have
/// tests asserting the message byte-for-byte. Change the wording here and
/// those tests must change with it — do not re-open-code the format string at
/// a call site.
///
/// # Errors
/// Returns an error when `manifest_path` has no parent directory (a filesystem
/// root or the empty path).
pub fn manifest_parent_dir(manifest_path: &Path) -> Result<&Path> {
    manifest_path
        .parent()
        .with_context(|| format!("Parent not found - {}", manifest_path.display()))
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
    let project_dir = manifest_parent_dir(project_manifest)?;
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
    // The substring fast reject cannot decide these: the literal
    // `.changepacks` IS present in the path text, so they fall through to the
    // component walk, which must still reject them because the only
    // occurrence sits inside a longer component.
    #[case("pkg/.changepacks.bak/file.json", false)]
    #[case("pkg/src/my.changepacks.rs", false)]
    // Regular paths return false
    #[case("src/index.js", false)]
    #[case("packages/core/package.json", false)]
    fn test_contains_changepacks_component(#[case] path: &str, #[case] expected: bool) {
        assert_eq!(contains_changepacks_component(Path::new(path)), expected);
    }

    /// One component that is deliberately not valid UTF-8, so `Path::to_str`
    /// on any path containing it returns `None`.
    #[cfg(any(unix, windows))]
    fn non_utf8_component() -> std::path::PathBuf {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            std::path::PathBuf::from(OsStr::from_bytes(&[0xff]))
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            // 0xD800 is an unpaired high surrogate: a legal Windows path unit
            // that has no UTF-8 encoding.
            std::path::PathBuf::from(std::ffi::OsString::from_wide(&[0xD800]))
        }
    }

    // A non-UTF-8 path makes the substring fast reject inapplicable, so it
    // must fall through to the component walk rather than answering `false`.
    #[cfg(any(unix, windows))]
    #[test]
    fn test_contains_changepacks_component_non_utf8_path_falls_through() {
        let path = non_utf8_component().join(".changepacks").join("log.json");
        assert!(path.to_str().is_none());
        assert!(contains_changepacks_component(&path));
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
    fn test_manifest_parent_dir_returns_containing_dir() {
        assert_eq!(
            manifest_parent_dir(Path::new("/project/package.json")).unwrap(),
            Path::new("/project")
        );
    }

    // The message is a pinned contract shared with
    // `changepacks_utils::is_workspace_by_sibling`; assert it byte-for-byte.
    #[test]
    fn test_manifest_parent_dir_errors_without_parent() {
        let err = manifest_parent_dir(Path::new("")).unwrap_err();
        assert_eq!(err.to_string(), "Parent not found - ");
    }

    #[test]
    fn test_should_mark_changed_errors_when_manifest_has_no_parent() {
        let candidate = Path::new("src/index.js");
        let manifest = Path::new("/");
        let err = should_mark_changed(candidate, manifest).unwrap_err();
        assert!(err.to_string().contains("Parent not found - /"));
    }
}
