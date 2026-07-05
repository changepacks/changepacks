use std::path::Path;

use anyhow::{Context, Result};

/// Decide whether a manifest roots a workspace via the shared
/// "declared field OR fixed sibling file" policy the Node and Dart finders
/// both apply.
///
/// `field_present` is the caller's already-computed check for the in-manifest
/// workspace declaration (`package.json`'s `workspaces`, `pubspec.yaml`'s
/// `workspace`). When it is `true` the manifest is a workspace outright and no
/// filesystem stat is issued. Otherwise the fixed `sibling_file`
/// (`pnpm-workspace.yaml`, `melos.yaml`) is stat'd next to the manifest; a stat
/// error (broken symlink, permission denied) is treated as "not a workspace",
/// matching the finders' previous `try_exists(...).unwrap_or(false)`
/// fallthrough-on-error behavior.
///
/// AGENTS.md rule: all file ops go through `tokio::fs`.
///
/// # Errors
/// Returns an error only when `manifest_path` has no parent directory (a root
/// or empty path), mirroring the `parent()` guard the finders used before this
/// helper was extracted.
pub async fn is_workspace_by_sibling(
    field_present: bool,
    manifest_path: &Path,
    sibling_file: &str,
) -> Result<bool> {
    if field_present {
        return Ok(true);
    }
    let sibling = manifest_path
        .parent()
        .with_context(|| format!("Parent not found - {}", manifest_path.display()))?
        .join(sibling_file);
    Ok(tokio::fs::try_exists(&sibling).await.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // `field_present == true` short-circuits to `Ok(true)` without touching
    // the filesystem — the sibling path is intentionally non-existent so a
    // stat would fail if it were (wrongly) issued.
    #[tokio::test]
    async fn test_field_present_short_circuits() {
        let result = is_workspace_by_sibling(
            true,
            &PathBuf::from("/nonexistent/dir/package.json"),
            "pnpm-workspace.yaml",
        )
        .await
        .unwrap();
        assert!(result);
    }

    // Sibling file present next to the manifest → workspace.
    #[tokio::test]
    async fn test_sibling_present() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("pubspec.yaml");
        tokio::fs::write(&manifest, "name: x").await.unwrap();
        let sibling = temp_dir.path().join("melos.yaml");
        tokio::fs::write(&sibling, "name: x").await.unwrap();

        let result = is_workspace_by_sibling(false, &manifest, "melos.yaml")
            .await
            .unwrap();
        assert!(result);

        temp_dir.close().unwrap();
    }

    // Sibling file absent → not a workspace (`try_exists` reports false).
    #[tokio::test]
    async fn test_sibling_absent() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("package.json");
        tokio::fs::write(&manifest, "{}").await.unwrap();

        let result = is_workspace_by_sibling(false, &manifest, "pnpm-workspace.yaml")
            .await
            .unwrap();
        assert!(!result);

        temp_dir.close().unwrap();
    }

    // A manifest path with no parent (the empty path) surfaces the
    // "Parent not found" error instead of silently reporting not-a-workspace.
    #[tokio::test]
    async fn test_no_parent_errors() {
        let result = is_workspace_by_sibling(false, Path::new(""), "melos.yaml").await;
        assert!(result.is_err());
    }
}
