use std::path::Path;

use anyhow::{Context, Result};

/// Whether a filesystem event on `candidate` should mark the project rooted at
/// `project_manifest` (its `package.json` / `Cargo.toml` / etc.) as changed.
///
/// Returns `false` for paths inside `.changepacks/` (changepack logs are not
/// user changes) and paths that fall outside the project's directory. This is
/// the byte-identical logic used by both `Package::check_changed` and
/// `Workspace::check_changed` defaults; extracted here so a future fix stays
/// applied to both trait defaults in one place.
///
/// # Errors
/// Returns error if `project_manifest` has no parent directory.
pub(crate) fn should_mark_changed(candidate: &Path, project_manifest: &Path) -> Result<bool> {
    if candidate.to_string_lossy().contains(".changepacks") {
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
}
