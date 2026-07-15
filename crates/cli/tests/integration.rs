use changepacks_core::{ChangePackLog, UpdateType};
use changepacks_utils::{
    collect_changepack_log_paths,
    test_support::{git_add_and_commit, init_git_repo, run_git},
};
use serial_test::serial;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct DirGuard {
    original: PathBuf,
}

impl DirGuard {
    fn change_to(path: &Path) -> Self {
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self { original }
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

/// Build a git-repo fixture in a fresh `TempDir` and return that `TempDir`.
///
/// Inits the repo on branch `main`, creates a `.changepacks/` directory, writes
/// each `(relative_path, content)` fixture (creating parent directories as
/// needed), then commits everything as "Initial commit". The caller keeps the
/// returned `TempDir` alive and re-derives `temp_path` via `temp_dir.path()`,
/// and installs its own `DirGuard` so the guard lives in the test's scope.
async fn setup_repo(files: &[(&str, &str)]) -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    write_repo_fixture(temp_dir.path(), files).await;
    temp_dir
}

/// Like [`setup_repo`], but builds the fixture on the canonicalized temp path
/// (avoids Windows path mismatches for git-based change detection). The caller
/// re-derives `temp_path` via `temp_dir.path().canonicalize().unwrap()`.
async fn setup_repo_canonical(files: &[(&str, &str)]) -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();
    write_repo_fixture(&temp_path, files).await;
    temp_dir
}

async fn write_repo_fixture(temp_path: &Path, files: &[(&str, &str)]) {
    init_git_repo(temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    for &(relative_path, content) in files {
        let full_path = temp_path.join(relative_path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(full_path, content).await.unwrap();
    }

    git_add_and_commit(temp_path, "Initial commit");
}

async fn read_pending_logs(repo_root: &Path) -> Vec<ChangePackLog> {
    let paths = collect_changepack_log_paths(&repo_root.join(".changepacks"))
        .await
        .unwrap();
    let mut logs = Vec::with_capacity(paths.len());
    for path in paths {
        let content = tokio::fs::read_to_string(path).await.unwrap();
        logs.push(serde_json::from_str(&content).unwrap());
    }
    logs
}

fn pending_changes(logs: &[ChangePackLog]) -> Vec<(PathBuf, UpdateType)> {
    let mut changes = logs
        .iter()
        .flat_map(|log| {
            log.changes()
                .iter()
                .map(|(path, update_type)| (path.clone(), *update_type))
        })
        .collect::<Vec<_>>();
    changes.sort();
    changes
}

async fn run_language_update(language: &str) -> anyhow::Result<()> {
    changepacks_cli::main(&[
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
        "--language".to_string(),
        language.to_string(),
        "--format".to_string(),
        "json".to_string(),
    ])
    .await
}

#[tokio::test]
#[serial]
async fn test_cli_init_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec![
        "changepacks".to_string(),
        "init".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok());
    assert!(!temp_path.join(".changepacks/config.json").exists());
}

#[tokio::test]
#[serial]
async fn test_cli_init_creates_config() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec!["changepacks".to_string(), "init".to_string()];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok());
    assert!(temp_path.join(".changepacks/config.json").exists());
}

#[tokio::test]
#[serial]
async fn test_cli_config() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec!["changepacks".to_string(), "config".to_string()];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_publish_dry_run() {
    // Override dry-run with `echo` so the test does not depend on a working
    // npm/registry environment.
    let temp_dir = setup_repo(&[
        (
            ".changepacks/config.json",
            r#"{"publishDryRun": {"node": "echo dry-run"}}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path();

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok());
}

/// Covers the `--dry-run` bail!() path in `handle_publish_with_prompter`:
/// when the underlying dry-run command exits non-zero, the CLI must surface
/// an error containing "Dry-run failed" so CI pipelines fail fast before
/// touching any registry.
#[tokio::test]
#[serial]
async fn test_cli_publish_dry_run_bails_on_failure() {
    // Force the dry-run command to exit non-zero so the loop records a failed
    // project and the handler hits the `Dry-run failed for ...` bail!().
    let fail_cmd = if cfg!(target_os = "windows") {
        "cmd /c exit 1"
    } else {
        "exit 1"
    };
    let config = format!(r#"{{"publishDryRun": {{"node": "{fail_cmd}"}}}}"#);
    let temp_dir = setup_repo(&[
        (".changepacks/config.json", config.as_str()),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path();

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_err(), "dry-run should fail when command exits 1");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Dry-run failed for"),
        "expected bail message, got: {err_msg}"
    );
    assert!(
        err_msg.contains("1 project(s)"),
        "expected failure count in message, got: {err_msg}"
    );
}

#[tokio::test]
#[serial]
async fn test_cli_publish_with_echo() {
    // Create config with echo publish command
    let temp_dir = setup_repo(&[
        (
            ".changepacks/config.json",
            r#"{"publish": {"node": "echo test"}}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path();

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_publish_no_projects() {
    let temp_dir = setup_repo(&[("README.md", "# Test")]).await;
    let temp_path = temp_dir.path();

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_publish_json_format() {
    // Override dry-run with `echo` so the test does not depend on a working
    // npm/registry environment.
    let temp_dir = setup_repo(&[
        (
            ".changepacks/config.json",
            r#"{"publishDryRun": {"node": "echo dry-run"}}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path();

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_update_with_changepack() {
    // Create changepacks directory and update log
    let temp_dir = setup_repo(&[
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path();

    let _dir_guard = DirGuard::change_to(temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok());

    // Verify version was updated
    let content = tokio::fs::read_to_string(temp_path.join("package.json"))
        .await
        .unwrap();
    assert!(content.contains("1.0.1"));
}

#[tokio::test]
#[serial]
async fn test_cli_check_basic() {
    // Canonicalize the path to avoid Windows path issues
    let temp_dir = setup_repo_canonical(&[(
        "package.json",
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec!["changepacks".to_string(), "check".to_string()];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok(), "check basic failed: {:?}", result.err());
}

#[tokio::test]
#[serial]
async fn test_cli_check_json_format() {
    let temp_dir = setup_repo_canonical(&[(
        "package.json",
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check json format failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_tree() {
    // Create multiple packages with workspace:* dependencies
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "root-pkg", "version": "1.0.0", "dependencies": {"child-pkg": "workspace:*"}}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        (
            "packages/child/package.json",
            r#"{"name": "child-pkg", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_ok(), "check tree failed: {:?}", result.err());
}

#[tokio::test]
#[serial]
async fn test_cli_check_tree_json_is_rejected_without_stdout() {
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "root-pkg", "version": "1.0.0", "dependencies": {"child-pkg": "workspace:*"}}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        (
            "packages/child/package.json",
            r#"{"name": "child-pkg", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();
    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/cli has a workspace root two levels up")
        .join("Cargo.toml");

    let output = std::process::Command::new(option_env!("CARGO").unwrap_or("cargo"))
        .args(["run", "--quiet", "-p", "changepacks", "--manifest-path"])
        .arg(&workspace_manifest)
        .args(["--", "check", "--tree", "--format", "json"])
        .current_dir(&temp_path)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run the changepacks binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "check --tree --format json should fail; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.is_empty(),
        "invalid tree/JSON output must not emit a text tree on stdout; got:\n{stdout}"
    );
    assert!(
        stderr.contains("`--tree` currently supports stdout output only"),
        "error should explain the supported tree format; got:\n{stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_filter_package() {
    let temp_dir = setup_repo_canonical(&[(
        "package.json",
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--filter".to_string(),
        "package".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check filter package failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_filter_workspace() {
    // Create a pnpm workspace
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "test-workspace", "version": "1.0.0"}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--filter".to_string(),
        "workspace".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check filter workspace failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_with_changepack_updates() {
    // Create changepacks directory and update log
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"package.json": "Minor"}, "note": "test feature", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        (
            "package.json",
            r#"{"name": "test-pkg", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec!["changepacks".to_string(), "check".to_string()];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check with changepack updates failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_no_projects() {
    let temp_dir = setup_repo_canonical(&[("README.md", "# Test")]).await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec!["changepacks".to_string(), "check".to_string()];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check no projects failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_changepacks_with_yes_and_message() {
    let temp_dir = setup_repo_canonical(&[(
        "package.json",
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Use --yes and -m to skip interactive prompts, --update-type to specify patch
    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "Test change message".to_string(),
        "--update-type".to_string(),
        "patch".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "changepacks with --yes and -m failed: {:?}",
        result.err()
    );

    // Verify a changepack log file was created
    let changepacks_dir = temp_path.join(".changepacks");
    let entries: Vec<_> = std::fs::read_dir(&changepacks_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("changepack_log_")
        })
        .collect();
    assert!(!entries.is_empty(), "No changepack log file was created");
}

// Regression: the default `changepacks` command must work in a repo that never
// ran `init`, i.e. one with NO `.changepacks/` directory. Previously the
// changepack-log write hard-failed with an OS error because the parent
// directory was missing. Inline the git setup here (instead of the
// `setup_repo*` helpers, which always create `.changepacks/`) so the fixture
// truly lacks the directory before the command runs.
#[tokio::test]
#[serial]
async fn test_cli_changepacks_creates_missing_changepacks_dir() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    // Write a package.json WITHOUT creating a `.changepacks/` directory.
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    // Sanity: the repo has no `.changepacks/` directory yet.
    assert!(
        !temp_path.join(".changepacks").exists(),
        "fixture should start without a .changepacks directory"
    );

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Use --yes and -m to skip interactive prompts, --update-type to specify patch.
    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "Missing dir message".to_string(),
        "--update-type".to_string(),
        "patch".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "changepacks should create the missing .changepacks dir: {:?}",
        result.err()
    );

    // Verify a changepack log file was created in the now-created directory.
    let changepacks_dir = temp_path.join(".changepacks");
    let entries: Vec<_> = std::fs::read_dir(&changepacks_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("changepack_log_")
        })
        .collect();
    assert!(!entries.is_empty(), "No changepack log file was created");
}

#[tokio::test]
#[serial]
async fn test_cli_changepacks_no_projects() {
    let temp_dir = setup_repo_canonical(&[("README.md", "# Test")]).await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // With --yes and no projects, it should print "No projects selected"
    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "Test message".to_string(),
        "--update-type".to_string(),
        "patch".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    // Should succeed but not create any log (no projects)
    assert!(
        result.is_ok(),
        "changepacks no projects failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_changepacks_empty_notes() {
    let temp_dir = setup_repo_canonical(&[(
        "package.json",
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // With empty message, should print "Notes are empty" and succeed
    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "".to_string(),
        "--update-type".to_string(),
        "patch".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "changepacks empty notes failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_changepacks_with_filter() {
    // Create a pnpm workspace
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "test-workspace", "version": "1.0.0"}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "Test filter".to_string(),
        "--update-type".to_string(),
        "minor".to_string(),
        "--filter".to_string(),
        "workspace".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "changepacks with filter failed: {:?}",
        result.err()
    );
}

// Test init error when config already exists
#[tokio::test]
#[serial]
async fn test_cli_init_already_initialized() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    // Create .changepacks/config.json first
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join(".changepacks/config.json"),
        r#"{"baseBranch": "main"}"#,
    )
    .await
    .unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec!["changepacks".to_string(), "init".to_string()];
    let result = changepacks_cli::main(&args).await;

    // Should fail because already initialized
    assert!(result.is_err());
}

// Test publish with language filter
#[tokio::test]
#[serial]
async fn test_cli_publish_with_language_filter() {
    // Override dry-run with `echo` so the test does not depend on a working
    // npm/registry environment (the real `npm publish --dry-run` fails under
    // tarpaulin / sandboxed CI because the package name conflicts with the
    // public registry).
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/config.json",
            r#"{"publishDryRun": {"node": "echo dry-run", "rust": "echo dry-run"}}"#,
        ),
        (
            "package.json",
            r#"{"name": "test-pkg", "version": "1.0.0"}"#,
        ),
        (
            "Cargo.toml",
            r#"[package]
name = "test-rust"
version = "1.0.0"
"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Only publish Node.js packages
    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
        "--language".to_string(),
        "node".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "publish with language filter failed: {:?}",
        result.err()
    );
}

// Test publish with project filter
#[tokio::test]
#[serial]
async fn test_cli_publish_with_project_filter() {
    // Override dry-run with `echo` so the test does not depend on a working
    // npm/registry environment.
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/config.json",
            r#"{"publishDryRun": {"node": "echo dry-run"}}"#,
        ),
        (
            "package.json",
            r#"{"name": "root-pkg", "version": "1.0.0"}"#,
        ),
        (
            "packages/core/package.json",
            r#"{"name": "core-pkg", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Only publish specific project
    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
        "--project".to_string(),
        "package.json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "publish with project filter failed: {:?}",
        result.err()
    );
}

// Test update with JSON format
#[tokio::test]
#[serial]
async fn test_cli_update_json_format() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--dry-run".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update JSON format failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_update_json_reports_pre_bump_version() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/cli has a workspace root two levels up")
        .join("Cargo.toml");

    let output = std::process::Command::new(option_env!("CARGO").unwrap_or("cargo"))
        .args(["run", "--quiet", "-p", "changepacks", "--manifest-path"])
        .arg(&workspace_manifest)
        .args(["--", "update", "--yes", "--format", "json"])
        .current_dir(&temp_path)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run the changepacks binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "update --format json exited non-zero ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains(r#""version": "1.0.0""#),
        "JSON output must report the pre-bump version; got:\n{stdout}"
    );
    assert!(
        stdout.contains(r#""nextVersion": "1.0.1""#),
        "JSON output must report the single-bumped next version; got:\n{stdout}"
    );
}

// Test update with language filter and JSON format clears applied changepack logs
#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_json_clears_logs() {
    // Given: a temp directory with a Rust package and a changepack log targeting it
    let temp_dir = setup_repo_canonical(&[
        (
            "Cargo.toml",
            "[package]\nname = \"rust-pkg\"\nversion = \"1.0.0\"\n",
        ),
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"Cargo.toml": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // When: running update with language filter (rust) and JSON format
    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
        "--language".to_string(),
        "rust".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    // Then: the command succeeds
    assert!(
        result.is_ok(),
        "update with language filter and JSON format failed: {:?}",
        result.err()
    );

    // And: the changepack log file should be removed (applied and cleared)
    let log_exists = tokio::fs::try_exists(temp_path.join(".changepacks/changepack_log_test.json"))
        .await
        .unwrap();
    assert!(
        !log_exists,
        "changepack log should be removed after applied update with language filter and JSON format"
    );

    // And: the Cargo.toml version should be bumped to 1.0.1 (Patch bump)
    let cargo_content = tokio::fs::read_to_string(temp_path.join("Cargo.toml"))
        .await
        .unwrap();
    assert!(
        cargo_content.contains("version = \"1.0.1\""),
        "Cargo.toml version should be bumped to 1.0.1, got: {}",
        cargo_content
    );
}

// Regression: `update --language <lang>` folds a workspace-inherited member
// (`version.workspace = true`) into its workspace-root entry, so the member's
// own path is no longer a key in `update_map`. The language-filtered
// `applied_paths` snapshot must still clear the member's changepack log (its
// bump is satisfied by the surviving workspace-root bump); otherwise the log is
// retained and re-applied — a double-bump — on the next `update`.
#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_clears_workspace_inherited_member_log() {
    let temp_dir = setup_repo_canonical(&[
        // Cargo workspace root owns the version via [workspace.package] and its own
        // [package]; a Patch bump promoted here must reach the member's log.
        (
            "Cargo.toml",
            r#"[workspace]
members = ["crates/foo"]

[workspace.package]
version = "2.5.0"

[package]
name = "root-pkg"
version = "2.5.0"
"#,
        ),
        // Member inherits its version from the workspace root.
        (
            "crates/foo/Cargo.toml",
            r#"[package]
name = "foo"
version.workspace = true
"#,
        ),
        // Changepack log targets the MEMBER path (folded into the root at update).
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"crates/foo/Cargo.toml": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // When: running update with the rust language filter (matches the workspace).
    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
        "--language".to_string(),
        "rust".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update --language rust failed: {:?}",
        result.err()
    );

    // Then: the member's log is cleared (its bump was satisfied by the
    // workspace-root bump that survived the rust filter).
    let log_exists = tokio::fs::try_exists(temp_path.join(".changepacks/changepack_log_test.json"))
        .await
        .unwrap();
    assert!(
        !log_exists,
        "workspace-inherited member's changepack log should be cleared after `update --language rust`"
    );

    // And: the workspace root version is bumped (2.5.0 -> 2.5.1).
    let cargo_content = tokio::fs::read_to_string(temp_path.join("Cargo.toml"))
        .await
        .unwrap();
    assert!(
        cargo_content.contains("version = \"2.5.1\""),
        "workspace root version should be bumped to 2.5.1, got: {}",
        cargo_content
    );
}

// Companion to the test above: when the language filter does NOT match the
// workspace root's language, the root is filtered out and never bumped, so the
// folded member's log MUST be retained (nothing satisfied its bump).
#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_retains_non_matching_member_log() {
    let temp_dir = setup_repo_canonical(&[
        (
            "Cargo.toml",
            r#"[workspace]
members = ["crates/foo"]

[workspace.package]
version = "2.5.0"

[package]
name = "root-pkg"
version = "2.5.0"
"#,
        ),
        (
            "crates/foo/Cargo.toml",
            r#"[package]
name = "foo"
version.workspace = true
"#,
        ),
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"crates/foo/Cargo.toml": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // When: the language filter (node) matches nothing in this all-rust workspace.
    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
        "--language".to_string(),
        "node".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update --language node failed: {:?}",
        result.err()
    );

    // Then: the rust member's log survives — no rust project was applied.
    let log_exists = tokio::fs::try_exists(temp_path.join(".changepacks/changepack_log_test.json"))
        .await
        .unwrap();
    assert!(
        log_exists,
        "rust member's changepack log must be retained when the language filter excludes rust"
    );

    // And: the workspace root is NOT bumped.
    let cargo_content = tokio::fs::read_to_string(temp_path.join("Cargo.toml"))
        .await
        .unwrap();
    assert!(
        cargo_content.contains("version = \"2.5.0\""),
        "workspace root version must stay 2.5.0 when node filter excludes it, got: {}",
        cargo_content
    );
}

#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_carries_update_on_bumps_exactly_once() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/config.json",
            r#"{"updateOn":{"crates/core/Cargo.toml":["bridge/node/package.json","bridge/python/pyproject.toml"]}}"#,
        ),
        (
            ".changepacks/changepack_log_core.json",
            r#"{"changes":{"crates/core/Cargo.toml":"Minor"},"note":"core feature","date":"2026-07-15T00:00:00Z"}"#,
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        ),
        (
            "bridge/node/package.json",
            r#"{"name":"bridge-node","version":"1.0.0"}"#,
        ),
        (
            "bridge/python/pyproject.toml",
            "[project]\nname = \"bridge-python\"\nversion = \"1.0.0\"\n",
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();
    let _dir_guard = DirGuard::change_to(&temp_path);

    run_language_update("rust").await.unwrap();

    let rust_manifest = tokio::fs::read_to_string(temp_path.join("crates/core/Cargo.toml"))
        .await
        .unwrap();
    assert!(rust_manifest.contains("version = \"1.1.0\""));
    assert_eq!(
        pending_changes(&read_pending_logs(&temp_path).await),
        vec![
            (PathBuf::from("bridge/node/package.json"), UpdateType::Patch,),
            (
                PathBuf::from("bridge/python/pyproject.toml"),
                UpdateType::Patch,
            ),
        ]
    );

    run_language_update("node").await.unwrap();

    let node_after_first_update = tokio::fs::read(temp_path.join("bridge/node/package.json"))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&node_after_first_update).contains("1.0.1"));
    assert_eq!(
        pending_changes(&read_pending_logs(&temp_path).await),
        vec![(
            PathBuf::from("bridge/python/pyproject.toml"),
            UpdateType::Patch,
        )]
    );

    run_language_update("python").await.unwrap();
    run_language_update("node").await.unwrap();
    run_language_update("python").await.unwrap();

    assert_eq!(
        tokio::fs::read(temp_path.join("bridge/node/package.json"))
            .await
            .unwrap(),
        node_after_first_update
    );
    let python_manifest = tokio::fs::read_to_string(temp_path.join("bridge/python/pyproject.toml"))
        .await
        .unwrap();
    assert!(python_manifest.contains("version = \"1.0.1\""));
    assert!(read_pending_logs(&temp_path).await.is_empty());
}

#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_carries_reverse_dependency_bump_exactly_once() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_core.json",
            r#"{"changes":{"crates/core/Cargo.toml":"Minor"},"note":"core feature","date":"2026-07-15T00:00:00Z"}"#,
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        ),
        (
            "bridge/node/package.json",
            r#"{"name":"bridge-node","version":"1.0.0","dependencies":{"core":"workspace:*"}}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();
    let _dir_guard = DirGuard::change_to(&temp_path);

    run_language_update("rust").await.unwrap();

    assert_eq!(
        pending_changes(&read_pending_logs(&temp_path).await),
        vec![(PathBuf::from("bridge/node/package.json"), UpdateType::Patch,)]
    );

    run_language_update("node").await.unwrap();
    let node_after_first_update = tokio::fs::read(temp_path.join("bridge/node/package.json"))
        .await
        .unwrap();
    run_language_update("node").await.unwrap();

    assert_eq!(
        tokio::fs::read(temp_path.join("bridge/node/package.json"))
            .await
            .unwrap(),
        node_after_first_update
    );
    assert!(String::from_utf8_lossy(&node_after_first_update).contains("1.0.1"));
    assert!(read_pending_logs(&temp_path).await.is_empty());
}

#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_update_on_transitive_descendant_bumps_once() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/config.json",
            r#"{"updateOn":{"crates/a/Cargo.toml":["bridge/node/package.json"],"bridge/node/package.json":["crates/c/Cargo.toml"]}}"#,
        ),
        (
            ".changepacks/changepack_log_a.json",
            r#"{"changes":{"crates/a/Cargo.toml":"Minor"},"note":"rust a feature","date":"2026-07-15T00:00:00Z"}"#,
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"rust-a\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        ),
        (
            "bridge/node/package.json",
            r#"{"name":"node-b","version":"1.0.0"}"#,
        ),
        (
            "crates/c/Cargo.toml",
            "[package]\nname = \"rust-c\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();
    let _dir_guard = DirGuard::change_to(&temp_path);

    run_language_update("rust").await.unwrap();
    let rust_c_after_materialization = tokio::fs::read(temp_path.join("crates/c/Cargo.toml"))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&rust_c_after_materialization).contains("1.0.1"));
    assert_eq!(
        pending_changes(&read_pending_logs(&temp_path).await),
        vec![(PathBuf::from("bridge/node/package.json"), UpdateType::Patch)]
    );

    run_language_update("node").await.unwrap();
    run_language_update("rust").await.unwrap();

    assert_eq!(
        tokio::fs::read(temp_path.join("crates/c/Cargo.toml"))
            .await
            .unwrap(),
        rust_c_after_materialization
    );
    let node_manifest = tokio::fs::read_to_string(temp_path.join("bridge/node/package.json"))
        .await
        .unwrap();
    assert!(node_manifest.contains("1.0.1"));
    assert!(read_pending_logs(&temp_path).await.is_empty());
}

#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_update_on_cycle_bumps_origin_once() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/config.json",
            r#"{"updateOn":{"crates/a/Cargo.toml":["bridge/node/package.json"],"bridge/node/package.json":["crates/a/Cargo.toml"]}}"#,
        ),
        (
            ".changepacks/changepack_log_a.json",
            r#"{"changes":{"crates/a/Cargo.toml":"Minor"},"note":"rust a feature","date":"2026-07-15T00:00:00Z"}"#,
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"rust-a\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        ),
        (
            "bridge/node/package.json",
            r#"{"name":"node-b","version":"1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();
    let _dir_guard = DirGuard::change_to(&temp_path);

    run_language_update("rust").await.unwrap();
    let rust_a_after_materialization = tokio::fs::read(temp_path.join("crates/a/Cargo.toml"))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&rust_a_after_materialization).contains("1.1.0"));

    run_language_update("node").await.unwrap();
    run_language_update("rust").await.unwrap();

    assert_eq!(
        tokio::fs::read(temp_path.join("crates/a/Cargo.toml"))
            .await
            .unwrap(),
        rust_a_after_materialization
    );
    assert!(read_pending_logs(&temp_path).await.is_empty());
}

#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_reverse_dependency_transitive_descendant_bumps_once() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_a.json",
            r#"{"changes":{"crates/a/Cargo.toml":"Minor"},"note":"rust a feature","date":"2026-07-15T00:00:00Z"}"#,
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"rust-a\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        ),
        (
            "bridge/node/package.json",
            r#"{"name":"node-b","version":"1.0.0","dependencies":{"rust-a":"workspace:*"}}"#,
        ),
        (
            "crates/c/Cargo.toml",
            "[package]\nname = \"rust-c\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[dependencies]\nnode-b = { package = \"node-b\", path = \"../../bridge/node\" }\n",
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();
    let _dir_guard = DirGuard::change_to(&temp_path);

    run_language_update("rust").await.unwrap();
    let rust_c_after_materialization = tokio::fs::read(temp_path.join("crates/c/Cargo.toml"))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&rust_c_after_materialization).contains("1.0.1"));
    assert_eq!(
        pending_changes(&read_pending_logs(&temp_path).await),
        vec![(PathBuf::from("bridge/node/package.json"), UpdateType::Patch)]
    );

    run_language_update("node").await.unwrap();
    run_language_update("rust").await.unwrap();

    assert_eq!(
        tokio::fs::read(temp_path.join("crates/c/Cargo.toml"))
            .await
            .unwrap(),
        rust_c_after_materialization
    );
    assert!(read_pending_logs(&temp_path).await.is_empty());
}

#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_reverse_dependency_cycle_bumps_origin_once() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_a.json",
            r#"{"changes":{"crates/a/Cargo.toml":"Minor"},"note":"rust a feature","date":"2026-07-15T00:00:00Z"}"#,
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"rust-a\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[dependencies]\nnode-b = { package = \"node-b\", path = \"../../bridge/node\" }\n",
        ),
        (
            "bridge/node/package.json",
            r#"{"name":"node-b","version":"1.0.0","dependencies":{"rust-a":"workspace:*"}}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();
    let _dir_guard = DirGuard::change_to(&temp_path);

    run_language_update("rust").await.unwrap();
    let rust_a_after_materialization = tokio::fs::read(temp_path.join("crates/a/Cargo.toml"))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&rust_a_after_materialization).contains("1.1.0"));

    run_language_update("node").await.unwrap();
    run_language_update("rust").await.unwrap();

    assert_eq!(
        tokio::fs::read(temp_path.join("crates/a/Cargo.toml"))
            .await
            .unwrap(),
        rust_a_after_materialization
    );
    assert!(read_pending_logs(&temp_path).await.is_empty());
}

#[tokio::test]
#[serial]
async fn test_cli_update_language_filter_does_not_duplicate_explicit_excluded_entry() {
    let original_note = "explicit bridge release note";
    let log = format!(
        r#"{{"changes":{{"crates/core/Cargo.toml":"Minor","bridge/node/package.json":"Major"}},"note":"{original_note}","date":"2026-07-15T00:00:00Z"}}"#
    );
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/config.json",
            r#"{"updateOn":{"crates/core/Cargo.toml":["bridge/node/package.json"]}}"#,
        ),
        (".changepacks/changepack_log_release.json", &log),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        ),
        (
            "bridge/node/package.json",
            r#"{"name":"bridge-node","version":"1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();
    let _dir_guard = DirGuard::change_to(&temp_path);

    run_language_update("rust").await.unwrap();

    let logs = read_pending_logs(&temp_path).await;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].note(), original_note);
    assert_eq!(
        pending_changes(&logs),
        vec![(PathBuf::from("bridge/node/package.json"), UpdateType::Major,)]
    );

    run_language_update("node").await.unwrap();

    let node_manifest = tokio::fs::read_to_string(temp_path.join("bridge/node/package.json"))
        .await
        .unwrap();
    assert!(node_manifest.contains("2.0.0"));
    assert!(read_pending_logs(&temp_path).await.is_empty());
}

// Test update with no updates found
#[tokio::test]
#[serial]
async fn test_cli_update_no_updates() {
    let temp_dir =
        setup_repo_canonical(&[("package.json", r#"{"name": "test", "version": "1.0.0"}"#)]).await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update no updates failed: {:?}",
        result.err()
    );
}

// Test update with JSON format and no updates
#[tokio::test]
#[serial]
async fn test_cli_update_json_no_updates() {
    let temp_dir =
        setup_repo_canonical(&[("package.json", r#"{"name": "test", "version": "1.0.0"}"#)]).await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update JSON no updates failed: {:?}",
        result.err()
    );
}

// Test check with changed files (hit line 72 in check.rs)
#[tokio::test]
#[serial]
async fn test_cli_check_with_changed_files() {
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "test-pkg", "version": "1.0.0"}"#,
        ),
        ("index.js", "console.log('hello');"),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    // Modify the file to make the project "changed"
    tokio::fs::write(temp_path.join("index.js"), "console.log('modified');")
        .await
        .unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec!["changepacks".to_string(), "check".to_string()];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check with changed files failed: {:?}",
        result.err()
    );
}

// Test check tree with complex dependency graph
#[tokio::test]
#[serial]
async fn test_cli_check_tree_complex_deps() {
    // Create a complex dependency structure with workspace:* dependencies
    // root -> pkg-a, pkg-b
    // pkg-a -> pkg-c
    // pkg-b -> pkg-c (diamond pattern)
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "root", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*", "pkg-b": "workspace:*"}}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        (
            "packages/pkg-a/package.json",
            r#"{"name": "pkg-a", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
        ),
        (
            "packages/pkg-b/package.json",
            r#"{"name": "pkg-b", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
        ),
        (
            "packages/pkg-c/package.json",
            r#"{"name": "pkg-c", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check tree complex deps failed: {:?}",
        result.err()
    );
}

// Test actual publish execution (not dry-run) with echo command
#[tokio::test]
#[serial]
async fn test_cli_publish_actual_execution() {
    // Create config with echo publish command
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/config.json",
            r#"{"publish": {"node": "echo publishing"}}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "publish actual execution failed: {:?}",
        result.err()
    );
}

// Test actual update execution (not dry-run)
#[tokio::test]
#[serial]
async fn test_cli_update_actual_execution() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"package.json": "Patch"}, "note": "test update", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update actual execution failed: {:?}",
        result.err()
    );

    // Verify version was updated
    let content = tokio::fs::read_to_string(temp_path.join("package.json"))
        .await
        .unwrap();
    assert!(
        content.contains("1.0.1"),
        "Version should be updated to 1.0.1"
    );

    // Verify changepack log was cleared
    let log_exists = temp_path
        .join(".changepacks/changepack_log_test.json")
        .exists();
    assert!(!log_exists, "Changepack log should be cleared after update");
}

// Regression: `changepacks update` must honor the configured `baseBranch`
// during its auxiliary (unfiltered) project walk. That second walk previously
// used `Config::default()` (baseBranch = "main"), so a repo whose configured
// baseBranch is `trunk` with NO `main` branch failed with
// `base branch 'main' not found in local refs`. Inline the git setup here
// (instead of `init_git_repo`, which hardcodes `-b main`) so the repo has only
// a `trunk` branch.
#[tokio::test]
#[serial]
async fn test_cli_update_respects_custom_base_branch() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    run_git(&temp_path, &["init", "-b", "trunk"]);
    run_git(&temp_path, &["config", "user.email", "test@test.com"]);
    run_git(&temp_path, &["config", "user.name", "Test"]);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join(".changepacks/config.json"),
        r#"{"baseBranch": "trunk"}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join(".changepacks/changepack_log_test.json"),
        r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update should honor custom baseBranch: {:?}",
        result.err()
    );

    // Version bumped 1.0.0 -> 1.0.1 proves the auxiliary walk completed instead
    // of erroring on a missing `main` ref.
    let content = tokio::fs::read_to_string(temp_path.join("package.json"))
        .await
        .unwrap();
    assert!(
        content.contains("1.0.1"),
        "Version should be updated to 1.0.1, got: {content}"
    );
}

// Test update with workspace dependencies
#[tokio::test]
#[serial]
async fn test_cli_update_with_workspace_deps() {
    let temp_dir = setup_repo_canonical(&[
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        ("package.json", r#"{"name": "root", "version": "1.0.0"}"#),
        (
            "packages/core/package.json",
            r#"{"name": "core", "version": "1.0.0"}"#,
        ),
        // cli package depends on core via workspace:*
        (
            "packages/cli/package.json",
            r#"{"name": "cli", "version": "1.0.0", "dependencies": {"core": "workspace:*"}}"#,
        ),
        // changepack log for core only
        (
            ".changepacks/changepack_log_core.json",
            r#"{"changes": {"packages/core/package.json": "Minor"}, "note": "update core", "date": "2025-01-01T00:00:00Z"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update with workspace deps failed: {:?}",
        result.err()
    );
}

// Test check tree with pending updates and changed files
#[tokio::test]
#[serial]
async fn test_cli_check_tree_with_updates_and_changes() {
    // Create packages with workspace:* dependencies
    // root -> pkg-a, pkg-b
    // pkg-a -> pkg-c
    // pkg-b -> pkg-c (diamond pattern)
    let temp_dir = setup_repo_canonical(&[
        // changepack log for one package
        (
            ".changepacks/changepack_log_update.json",
            r#"{"changes": {"packages/pkg-a/package.json": "Minor"}, "note": "update pkg-a", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        (
            "package.json",
            r#"{"name": "root", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*", "pkg-b": "workspace:*"}}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        (
            "packages/pkg-a/package.json",
            r#"{"name": "pkg-a", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
        ),
        ("packages/pkg-a/index.js", "// initial"),
        (
            "packages/pkg-b/package.json",
            r#"{"name": "pkg-b", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
        ),
        (
            "packages/pkg-c/package.json",
            r#"{"name": "pkg-c", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    // Modify pkg-a to make it "changed"
    tokio::fs::write(temp_path.join("packages/pkg-a/index.js"), "// modified")
        .await
        .unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check tree with updates and changes failed: {:?}",
        result.err()
    );
}

// Test check tree with orphaned project (no dependencies)
#[tokio::test]
#[serial]
async fn test_cli_check_tree_with_orphan() {
    let temp_dir = setup_repo_canonical(&[
        // one package with workspace:* deps, one orphaned
        (
            "package.json",
            r#"{"name": "root", "version": "1.0.0", "dependencies": {"child": "workspace:*"}}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        (
            "packages/child/package.json",
            r#"{"name": "child", "version": "1.0.0"}"#,
        ),
        // an orphaned package (not in any dependency chain)
        (
            "packages/orphan/package.json",
            r#"{"name": "orphan", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check tree with orphan failed: {:?}",
        result.err()
    );
}

// Regression: `check --tree` must display EVERY project even when two DISTINCT
// projects share a name. `name_to_project` keeps only the last-inserted project
// per name, so the tree walk renders just one of a same-named pair; the orphan
// pass must therefore key on the printed identity (the unique manifest path via
// `line_cache`), not the name, or the other same-named project is silently
// dropped. Flat `check` shows both, so `--tree` must too. Fixture: a Node `core`
// and a Rust `core`.
//
// `check --tree` emits its listing through `println!`, which writes to the
// process stdout — uncapturable in-process on stable without an extra crate — so
// observe it through the real workspace `changepacks` binary's captured stdout.
// `--quiet` keeps cargo's own progress on stderr; `NO_COLOR` strips ANSI styling
// so the manifest-path substrings match verbatim.
#[tokio::test]
#[serial]
async fn test_cli_check_tree_shows_both_same_named_projects() {
    let temp_dir = setup_repo_canonical(&[
        (
            "packages/core/package.json",
            r#"{"name": "core", "version": "1.0.0"}"#,
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\n",
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // The binary lives in the workspace `changepacks` crate (two levels up from
    // this crate's manifest); run it against the fixture repo via its cwd.
    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/cli has a workspace root two levels up")
        .join("Cargo.toml");

    let output = std::process::Command::new(option_env!("CARGO").unwrap_or("cargo"))
        .args(["run", "--quiet", "-p", "changepacks", "--manifest-path"])
        .arg(&workspace_manifest)
        .args(["--", "check", "--tree"])
        .current_dir(&temp_path)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run the changepacks binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "check --tree exited non-zero ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );

    // Normalize separators so the assertion holds on Windows (`\`) and Unix (`/`).
    let normalized = stdout.replace('\\', "/");
    assert!(
        normalized.contains("packages/core/package.json"),
        "tree output must list the Node `core` at packages/core/package.json; got:\n{stdout}"
    );
    assert!(
        normalized.contains("crates/core/Cargo.toml"),
        "tree output must list the Rust `core` at crates/core/Cargo.toml; got:\n{stdout}"
    );
}

// Test publish with failing command (to cover error path)
#[tokio::test]
#[serial]
async fn test_cli_publish_with_failing_command() {
    // Create config with failing publish command
    let fail_cmd = if cfg!(target_os = "windows") {
        r#"{"publish": {"node": "cmd /c exit 1"}}"#
    } else {
        r#"{"publish": {"node": "exit 1"}}"#
    };
    let temp_dir = setup_repo_canonical(&[
        (".changepacks/config.json", fail_cmd),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    // Should return error since publish failed (exit code propagation)
    assert!(
        result.is_err(),
        "publish with failing command should return error for non-zero exit code"
    );
}

// Test check tree with circular dependencies (covers check.rs lines 174-176 - orphan display)
// When A depends on B and B depends on A, neither is a root, so both become orphans
#[tokio::test]
#[serial]
async fn test_cli_check_tree_circular_deps() {
    // Create circular dependency: pkg-a -> pkg-b, pkg-b -> pkg-a
    // Neither is a root (both are in has_dependencies), so both become orphans
    let temp_dir = setup_repo_canonical(&[
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        ("package.json", r#"{"name": "root", "version": "1.0.0"}"#),
        (
            "packages/pkg-a/package.json",
            r#"{"name": "pkg-a", "version": "1.0.0", "dependencies": {"pkg-b": "workspace:*"}}"#,
        ),
        (
            "packages/pkg-b/package.json",
            r#"{"name": "pkg-b", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*"}}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check tree circular deps failed: {:?}",
        result.err()
    );
}

// Test publish with JSON format and no projects (covers publish.rs lines 83-84)
#[tokio::test]
#[serial]
async fn test_cli_publish_json_no_projects() {
    let temp_dir = setup_repo_canonical(&[("README.md", "# Test")]).await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "publish json no projects failed: {:?}",
        result.err()
    );
}

// Test check tree with deeply nested dependencies (covers check.rs lines 216-250)
#[tokio::test]
#[serial]
async fn test_cli_check_tree_deeply_nested() {
    // Create a deep dependency chain with workspace:* deps: root -> a -> b -> c -> d
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "root", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*"}}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        (
            "packages/pkg-a/package.json",
            r#"{"name": "pkg-a", "version": "1.0.0", "dependencies": {"pkg-b": "workspace:*"}}"#,
        ),
        (
            "packages/pkg-b/package.json",
            r#"{"name": "pkg-b", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
        ),
        (
            "packages/pkg-c/package.json",
            r#"{"name": "pkg-c", "version": "1.0.0", "dependencies": {"pkg-d": "workspace:*"}}"#,
        ),
        (
            "packages/pkg-d/package.json",
            r#"{"name": "pkg-d", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check tree deeply nested failed: {:?}",
        result.err()
    );
}

// Test check tree where a dependency is visited multiple times (covers check.rs lines 237-252)
// This test specifically ensures that an already-visited dep that is NOT the last dep hits line 240 (├── branch)
#[tokio::test]
#[serial]
async fn test_cli_check_tree_shared_dep_visited_twice() {
    // Create packages where shared-dep is depended on by multiple packages
    // root1 -> shared-dep (visits shared-dep first)
    // root2 -> [shared-dep, z-pkg] (shared-dep is NOT last after sorting, hits line 240)
    // Both root1 and root2 are root nodes
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "root1", "version": "1.0.0", "dependencies": {"shared-dep": "workspace:*"}}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        // root2 depends on both shared-dep and z-pkg. After sorting: [shared-dep, z-pkg]
        // shared-dep is idx=0 (not last), so when already visited, hits line 240 (├──)
        (
            "packages/root2/package.json",
            r#"{"name": "root2", "version": "1.0.0", "dependencies": {"shared-dep": "workspace:*", "z-pkg": "workspace:*"}}"#,
        ),
        (
            "packages/shared-dep/package.json",
            r#"{"name": "shared-dep", "version": "1.0.0"}"#,
        ),
        (
            "packages/z-pkg/package.json",
            r#"{"name": "z-pkg", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check tree shared dep visited twice failed: {:?}",
        result.err()
    );
}

// Test changepacks with package filter (covers changepacks.rs line 41)
#[tokio::test]
#[serial]
async fn test_cli_changepacks_with_package_filter() {
    // Create a workspace and a package
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "root-workspace", "version": "1.0.0"}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        (
            "packages/pkg/package.json",
            r#"{"name": "pkg", "version": "1.0.0"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Use --filter package to only select packages (not workspaces)
    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "Package only update".to_string(),
        "--update-type".to_string(),
        "patch".to_string(),
        "--filter".to_string(),
        "package".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "changepacks with package filter failed: {:?}",
        result.err()
    );
}

// Test update dry-run with JSON format (covers update.rs lines 102-103)
#[tokio::test]
#[serial]
async fn test_cli_update_dry_run_json() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--dry-run".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update dry-run json failed: {:?}",
        result.err()
    );
}

// Test update dry-run with stdout format (covers update.rs lines 99-100)
#[tokio::test]
#[serial]
async fn test_cli_update_dry_run_stdout() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_test.json",
            r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Use default stdout format with dry-run (not JSON)
    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update dry-run stdout failed: {:?}",
        result.err()
    );
}

// Test update with workspace in update list (covers update.rs line 141)
#[tokio::test]
#[serial]
async fn test_cli_update_with_workspace_only() {
    let temp_dir = setup_repo_canonical(&[
        ("pnpm-workspace.yaml", "packages:\n  - packages/*"),
        (
            "package.json",
            r#"{"name": "root-workspace", "version": "1.0.0"}"#,
        ),
        // changepack log for the workspace
        (
            ".changepacks/changepack_log_ws.json",
            r#"{"changes": {"package.json": "Minor"}, "note": "update workspace", "date": "2025-01-01T00:00:00Z"}"#,
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update with workspace only failed: {:?}",
        result.err()
    );
}

// Test changepacks without --update-type (covers changepacks.rs line 54)
#[tokio::test]
#[serial]
async fn test_cli_changepacks_without_update_type() {
    let temp_dir = setup_repo_canonical(&[(
        "package.json",
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Run without --update-type, so it will iterate Major, Minor, Patch
    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "Test without update type".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "changepacks without update type failed: {:?}",
        result.err()
    );
}

// Test publish stdout with failing command (covers publish.rs line 149)
#[tokio::test]
#[serial]
async fn test_cli_publish_stdout_failing() {
    // Create config with failing publish command
    let fail_cmd = if cfg!(target_os = "windows") {
        r#"{"publish": {"node": "cmd /c exit 1"}}"#
    } else {
        r#"{"publish": {"node": "exit 1"}}"#
    };
    let temp_dir = setup_repo_canonical(&[
        (".changepacks/config.json", fail_cmd),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Use stdout format (default) to hit the error eprintln! path
    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    // Publishing fails so command should return error (non-zero exit code)
    assert!(
        result.is_err(),
        "publish stdout failing should return error for non-zero exit code"
    );
}

// Tests for interactive code paths using MockPrompter
mod interactive_tests {
    use super::*;
    use changepacks_cli::commands::{
        ChangepackArgs, PublishArgs, UpdateArgs, handle_changepack_with_prompter,
        handle_publish_with_prompter, handle_update_with_prompter,
    };
    use changepacks_cli::options::FormatOptions;
    use changepacks_cli::prompter::MockPrompter;

    // Test publish cancelled (covers publish.rs lines 116-124)
    #[tokio::test]
    #[serial]
    async fn test_publish_cancelled_stdout() {
        let temp_dir =
            setup_repo_canonical(&[("package.json", r#"{"name": "test", "version": "1.0.0"}"#)])
                .await;
        let temp_path = temp_dir.path().canonicalize().unwrap();

        let _dir_guard = DirGuard::change_to(&temp_path);

        let args = PublishArgs {
            dry_run: false,
            yes: false, // Not auto-confirm, will use prompter
            format: FormatOptions::Stdout,
            remote: false,
            language: vec![],
            project: vec![],
        };

        // MockPrompter with confirm_value = false (cancelled)
        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };

        let result = handle_publish_with_prompter(&args, &prompter).await;
        assert!(result.is_ok(), "publish cancelled should succeed");
    }

    // Test publish cancelled with JSON format (covers publish.rs lines 120-122)
    #[tokio::test]
    #[serial]
    async fn test_publish_cancelled_json() {
        let temp_dir =
            setup_repo_canonical(&[("package.json", r#"{"name": "test", "version": "1.0.0"}"#)])
                .await;
        let temp_path = temp_dir.path().canonicalize().unwrap();

        let _dir_guard = DirGuard::change_to(&temp_path);

        let args = PublishArgs {
            dry_run: false,
            yes: false,
            format: FormatOptions::Json,
            remote: false,
            language: vec![],
            project: vec![],
        };

        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };

        let result = handle_publish_with_prompter(&args, &prompter).await;
        assert!(result.is_ok(), "publish cancelled json should succeed");
    }

    // Test update cancelled (covers update.rs lines 115-123)
    #[tokio::test]
    #[serial]
    async fn test_update_cancelled_stdout() {
        let temp_dir = setup_repo_canonical(&[
            (
                ".changepacks/changepack_log_test.json",
                r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
            ),
            ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
        ])
        .await;
        let temp_path = temp_dir.path().canonicalize().unwrap();

        let _dir_guard = DirGuard::change_to(&temp_path);

        let args = UpdateArgs {
            dry_run: false,
            yes: false,
            format: FormatOptions::Stdout,
            remote: false,
            language: vec![],
        };

        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };

        let result = handle_update_with_prompter(&args, &prompter).await;
        assert!(result.is_ok(), "update cancelled should succeed");
    }

    // Test update cancelled with JSON format (covers update.rs lines 119-121)
    #[tokio::test]
    #[serial]
    async fn test_update_cancelled_json() {
        let temp_dir = setup_repo_canonical(&[
            (
                ".changepacks/changepack_log_test.json",
                r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
            ),
            ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
        ])
        .await;
        let temp_path = temp_dir.path().canonicalize().unwrap();

        let _dir_guard = DirGuard::change_to(&temp_path);

        let args = UpdateArgs {
            dry_run: false,
            yes: false,
            format: FormatOptions::Json,
            remote: false,
            language: vec![],
        };

        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };

        let result = handle_update_with_prompter(&args, &prompter).await;
        assert!(result.is_ok(), "update cancelled json should succeed");
    }

    // Test changepacks with interactive selection (covers changepacks.rs lines 61-95)
    #[tokio::test]
    #[serial]
    async fn test_changepacks_interactive_select() {
        let temp_dir =
            setup_repo_canonical(&[("package.json", r#"{"name": "test", "version": "1.0.0"}"#)])
                .await;
        let temp_path = temp_dir.path().canonicalize().unwrap();

        let _dir_guard = DirGuard::change_to(&temp_path);

        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: false,                                // Use interactive mode
            message: Some("test message".to_string()), // Provide message to skip text prompt
            update_type: None,                         // Will iterate through Major, Minor, Patch
            language: vec![],
        };

        let prompter = MockPrompter {
            select_all: true,
            confirm_value: true,
            text_value: "test note".to_string(),
        };

        let result = handle_changepack_with_prompter(&args, &prompter).await;
        assert!(result.is_ok(), "changepacks interactive should succeed");
    }

    // Test changepacks with no selection (covers changepacks.rs empty selection path)
    #[tokio::test]
    #[serial]
    async fn test_changepacks_no_selection() {
        let temp_dir =
            setup_repo_canonical(&[("package.json", r#"{"name": "test", "version": "1.0.0"}"#)])
                .await;
        let temp_path = temp_dir.path().canonicalize().unwrap();

        let _dir_guard = DirGuard::change_to(&temp_path);

        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: false,
            message: Some("test".to_string()),
            update_type: None,
            language: vec![],
        };

        let prompter = MockPrompter {
            select_all: false, // Select nothing
            confirm_value: true,
            text_value: "test note".to_string(),
        };

        let result = handle_changepack_with_prompter(&args, &prompter).await;
        assert!(result.is_ok(), "changepacks no selection should succeed");
    }

    // Test changepacks with text prompt (covers changepacks.rs line 133)
    #[tokio::test]
    #[serial]
    async fn test_changepacks_text_prompt() {
        let temp_dir =
            setup_repo_canonical(&[("package.json", r#"{"name": "test", "version": "1.0.0"}"#)])
                .await;
        let temp_path = temp_dir.path().canonicalize().unwrap();

        let _dir_guard = DirGuard::change_to(&temp_path);

        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: true,     // Auto-select all
            message: None, // No message, will use text prompt
            update_type: Some(changepacks_core::UpdateType::Patch),
            language: vec![],
        };

        let prompter = MockPrompter {
            select_all: true,
            confirm_value: true,
            text_value: "prompted note".to_string(),
        };

        let result = handle_changepack_with_prompter(&args, &prompter).await;
        assert!(result.is_ok(), "changepacks text prompt should succeed");
    }

    // Test changepacks with changed project in interactive mode (covers changepacks.rs line 77)
    // Line 77 is `Some(index)` when project.is_changed() returns true
    #[tokio::test]
    #[serial]
    async fn test_changepacks_interactive_with_changed_project() {
        let temp_dir = setup_repo_canonical(&[
            ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
            ("index.js", "// initial"),
        ])
        .await;
        let temp_path = temp_dir.path().canonicalize().unwrap();

        // Modify a file to make the project "changed"
        tokio::fs::write(temp_path.join("index.js"), "// modified")
            .await
            .unwrap();

        let _dir_guard = DirGuard::change_to(&temp_path);

        // Use interactive mode with update_type: None (will iterate Major, Minor, Patch)
        // The changed project should be detected and line 77 will be hit
        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: false, // Interactive mode
            message: Some("test message".to_string()),
            update_type: None, // Will iterate through all update types
            language: vec![],
        };

        let prompter = MockPrompter {
            select_all: true,
            confirm_value: true,
            text_value: "test note".to_string(),
        };

        let result = handle_changepack_with_prompter(&args, &prompter).await;
        assert!(
            result.is_ok(),
            "changepacks with changed project should succeed"
        );
    }
}

// --- Language filter integration tests ---

#[tokio::test]
#[serial]
async fn test_cli_check_with_language_filter() {
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "node-pkg", "version": "1.0.0"}"#,
        ),
        (
            "Cargo.toml",
            "[package]\nname = \"rust-pkg\"\nversion = \"1.0.0\"\n",
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Filter check to only Node.js
    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--language".to_string(),
        "node".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "check with language filter failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_update_with_language_filter() {
    let temp_dir = setup_repo_canonical(&[
        (
            ".changepacks/changepack_log_lang.json",
            r#"{"changes": {"package.json": "Patch"}, "note": "test", "date": "2025-01-01T00:00:00Z"}"#,
        ),
        ("package.json", r#"{"name": "node-pkg", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Filter update to only Rust (should filter out the Node package update)
    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
        "--language".to_string(),
        "rust".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "update with language filter failed: {:?}",
        result.err()
    );

    // Verify version was NOT updated (filtered out by language)
    let content = tokio::fs::read_to_string(temp_path.join("package.json"))
        .await
        .unwrap();
    assert!(
        content.contains("1.0.0"),
        "Node package should not be updated when filtering by Rust"
    );
}

#[tokio::test]
#[serial]
async fn test_cli_changepacks_with_language_filter() {
    let temp_dir = setup_repo_canonical(&[
        (
            "package.json",
            r#"{"name": "node-pkg", "version": "1.0.0"}"#,
        ),
        (
            "Cargo.toml",
            "[package]\nname = \"rust-pkg\"\nversion = \"1.0.0\"\n",
        ),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    // Filter to only Node.js and create changepack
    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "Test language filter".to_string(),
        "--update-type".to_string(),
        "patch".to_string(),
        "--language".to_string(),
        "node".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(
        result.is_ok(),
        "changepacks with language filter failed: {:?}",
        result.err()
    );
}

// Test publish with stderr output in stdout format (covers publish.rs line 128 - stderr branch in print_publish_output)
#[tokio::test]
#[serial]
async fn test_cli_publish_stdout_failing_with_stderr() {
    let fail_cmd = if cfg!(target_os = "windows") {
        r#"{"publish": {"node": "echo error_output 1>&2 & exit 1"}}"#
    } else {
        r#"{"publish": {"node": "echo error_output >&2; exit 1"}}"#
    };
    let temp_dir = setup_repo_canonical(&[
        (".changepacks/config.json", fail_cmd),
        ("package.json", r#"{"name": "test", "version": "1.0.0"}"#),
    ])
    .await;
    let temp_path = temp_dir.path().canonicalize().unwrap();

    let _dir_guard = DirGuard::change_to(&temp_path);

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    assert!(result.is_err(), "publish with stderr should fail");
}
