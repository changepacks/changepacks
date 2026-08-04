use std::path::Path;

use anyhow::Result;
use changepacks_core::manifest_parent_dir;

/// Decide whether a manifest roots a workspace via the shared
/// "declared field OR fixed sibling file" policy the Node and Dart finders
/// both apply.
///
/// `field_present` is the caller's already-computed check for the in-manifest
/// workspace declaration (`package.json`'s `workspaces`, `pubspec.yaml`'s
/// `workspace`). When it is `true` the manifest is a workspace outright and no
/// filesystem stat is issued. Otherwise the fixed `sibling_file`
/// (`pnpm-workspace.yaml`, `melos.yaml`) is checked next to the manifest via the
/// shared `changepacks_core::is_regular_file` marker-file discipline: only a
/// *regular file* counts, so a directory bearing that name is rejected (not a
/// workspace). Missing sibling files also return false, while other metadata
/// errors are propagated.
///
/// AGENTS.md rule: all file ops go through `tokio::fs`.
///
/// # Errors
/// Returns an error when `manifest_path` has no parent directory (a root or
/// empty path) — the `"Parent not found - {}"` context comes from the shared
/// `changepacks_core::manifest_parent_dir`, which owns that message — or when
/// sibling metadata cannot be read for a reason other than the sibling not
/// existing.
pub async fn is_workspace_by_sibling(
    field_present: bool,
    manifest_path: &Path,
    sibling_file: &str,
) -> Result<bool> {
    if field_present {
        return Ok(true);
    }
    let sibling = manifest_parent_dir(manifest_path)?.join(sibling_file);
    changepacks_core::is_regular_file(&sibling).await
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

    // Sibling file absent → not a workspace (`is_regular_file` reports false).
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

    // A *directory* named like the sibling marker must NOT be treated as a
    // workspace: `is_regular_file` rejects directories, unlike the bare
    // `try_exists` this function used before.
    #[tokio::test]
    async fn test_sibling_directory_is_not_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = temp_dir.path().join("pubspec.yaml");
        tokio::fs::write(&manifest, "name: x").await.unwrap();
        tokio::fs::create_dir(temp_dir.path().join("melos.yaml"))
            .await
            .unwrap();

        let result = is_workspace_by_sibling(false, &manifest, "melos.yaml")
            .await
            .unwrap();
        assert!(!result);

        temp_dir.close().unwrap();
    }

    // A sibling stat that fails for a reason *other* than "not found" must be
    // propagated, not swallowed back into `Ok(false)` — that is exactly the
    // `try_exists` shape this function moved away from. Rooting the manifest
    // *inside* a regular file makes the sibling stat traverse a non-directory,
    // which Unix reports as `ENOTDIR`. Gated on unix because Windows maps the
    // same shape to `NotFound`, which `regular_file_metadata` legitimately
    // normalizes to `Ok(false)`.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_sibling_metadata_error_is_propagated() {
        let temp_dir = TempDir::new().unwrap();
        let blocker = temp_dir.path().join("blocker");
        tokio::fs::write(&blocker, "not a directory").await.unwrap();
        let manifest = blocker.join("package.json");

        let err = is_workspace_by_sibling(false, &manifest, "pnpm-workspace.yaml")
            .await
            .expect_err("a non-NotFound sibling stat failure must be propagated");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Failed to read metadata for"),
            "expected metadata-read context in error, got: {msg}"
        );

        temp_dir.close().unwrap();
    }

    // A manifest path with no parent (the empty path) surfaces the
    // "Parent not found" error instead of silently reporting not-a-workspace.
    // Pin the documented message so the branch cannot silently lose its
    // context and degrade into a bare io error.
    #[tokio::test]
    async fn test_no_parent_errors() {
        let err = is_workspace_by_sibling(false, Path::new(""), "melos.yaml")
            .await
            .expect_err("a parentless manifest path must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Parent not found"),
            "expected parent-not-found context in error, got: {msg}"
        );
    }
}
