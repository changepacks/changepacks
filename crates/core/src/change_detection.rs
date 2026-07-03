use std::ffi::OsStr;
use std::path::{Component, Path};

use anyhow::{Context, Result};

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
    if candidate
        .components()
        .any(|c| matches!(c, Component::Normal(name) if name == OsStr::new(".changepacks")))
    {
        return Ok(false);
    }
    let project_dir = project_manifest.parent().context("Parent not found")?;
    Ok(candidate.starts_with(project_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_mark_changed_true_for_file_in_project() {
        let candidate = Path::new("/project/src/index.js");
        let manifest = Path::new("/project/package.json");
        assert!(should_mark_changed(candidate, manifest).unwrap());
    }

    #[test]
    fn test_should_mark_changed_false_for_changepacks_path() {
        let candidate = Path::new("/project/.changepacks/change.json");
        let manifest = Path::new("/project/package.json");
        assert!(!should_mark_changed(candidate, manifest).unwrap());
    }

    #[test]
    fn test_should_mark_changed_false_for_file_outside_project() {
        let candidate = Path::new("/other-project/src/index.js");
        let manifest = Path::new("/project/package.json");
        assert!(!should_mark_changed(candidate, manifest).unwrap());
    }

    #[test]
    fn test_should_mark_changed_errors_when_manifest_has_no_parent() {
        let candidate = Path::new("src/index.js");
        let manifest = Path::new("");
        let err = should_mark_changed(candidate, manifest).unwrap_err();
        assert!(err.to_string().contains("Parent not found"));
    }

    #[test]
    fn test_should_mark_changed_ignores_sibling_dot_prefixed_dirs() {
        // Sibling directory whose name only *contains* ".changepacks" as a
        // substring must NOT be swallowed by the guard. The old substring
        // check treated any of these as "changepack log — do not mark
        // changed", silently dropping legitimate source edits.
        let manifest = Path::new("/project/package.json");

        // A file inside a sibling `.changepacks-backup/` directory: this
        // is user data, not a changepack log, so the project should be
        // marked changed.
        let backup_candidate = Path::new("/project/.changepacks-backup/pinned.json");
        assert!(should_mark_changed(backup_candidate, manifest).unwrap());

        // A file whose name literally contains `.changepacks` — again
        // real user data, should be marked changed.
        let named_candidate = Path::new("/project/notes-about-.changepacks.md");
        assert!(should_mark_changed(named_candidate, manifest).unwrap());

        // Regression: the true `.changepacks/` directory case still
        // returns `false`, so the fix is a strict widening — nothing that
        // previously passed silently now leaks through.
        let real_log = Path::new("/project/.changepacks/change.json");
        assert!(!should_mark_changed(real_log, manifest).unwrap());
    }
}
