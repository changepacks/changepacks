use std::path::Path;

use anyhow::{Context, Result};
use changepacks_core::UpdateType;

use crate::next_version_or_default;

/// Compute, write, and store the next manifest version through a crate-specific writer.
///
/// # Errors
/// Returns an error when semver calculation fails or the writer fails.
pub async fn bump_version_with<W: AsyncFnOnce(&str) -> Result<()>>(
    version: &mut Option<String>,
    path: &Path,
    update_type: UpdateType,
    write: W,
) -> Result<()> {
    let new_version = next_version_or_default(version.as_deref(), update_type)
        .with_context(|| format!("Failed to compute next version for {}", path.display()))?;
    write(&new_version).await?;
    *version = Some(new_version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use changepacks_core::UpdateType;

    use super::bump_version_with;

    #[tokio::test]
    async fn test_bump_version_with_writes_and_stores_new_version() {
        let path = Path::new("/tmp/changepacks-utils/package.json");
        let mut version = Some("1.2.3".to_string());
        let mut written = String::new();

        bump_version_with(&mut version, path, UpdateType::Minor, async |new_version| {
            written = new_version.to_string();
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(version.as_deref(), Some("1.3.0"));
        assert_eq!(written, "1.3.0");
    }

    #[tokio::test]
    async fn test_bump_version_with_writer_error_leaves_version_untouched() {
        let path = Path::new("/tmp/changepacks-utils/writer-fail/package.json");
        let mut version = Some(String::from("1.2.3"));

        let err = bump_version_with(&mut version, path, UpdateType::Minor, async |_| {
            Err(anyhow::anyhow!("boom"))
        })
        .await
        .expect_err("writer failure must propagate");
        let chain = format!("{err:#}");

        assert!(
            chain.contains("boom"),
            "error chain should surface the writer error, got: {chain}"
        );
        assert_eq!(
            version.as_deref(),
            Some("1.2.3"),
            "a failed write must leave the in-memory version untouched"
        );
    }

    /// Single owner of the "bump error names the manifest path" assertion: the
    /// context is added by `bump_version_with` itself, so every language crate
    /// inherits it and must not re-test it locally.
    #[tokio::test]
    async fn test_bump_version_with_bump_error_includes_path() {
        let path = Path::new("/nonexistent/utils-bump/package.json");
        let mut version = Some("abc".to_string());
        let err = bump_version_with(&mut version, path, UpdateType::Patch, async |_| Ok(()))
            .await
            .expect_err("malformed version must fail before writing");
        let chain = format!("{err:#}");

        assert!(
            chain.contains(&path.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }
}
