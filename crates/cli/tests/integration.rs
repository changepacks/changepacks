use serial_test::serial;
use std::path::Path;
use tempfile::TempDir;

fn init_git_repo(path: &Path) {
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .unwrap();
}

fn git_add_and_commit(path: &Path, message: &str) {
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(path)
        .output()
        .unwrap();
}

#[tokio::test]
#[serial]
async fn test_cli_init_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "init".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok());
    assert!(!temp_path.join(".changepacks/config.json").exists());
}

#[tokio::test]
#[serial]
async fn test_cli_init_creates_config() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp_path).unwrap();

    let args = vec!["changepacks".to_string(), "init".to_string()];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok());
    assert!(temp_path.join(".changepacks/config.json").exists());
}

#[tokio::test]
#[serial]
async fn test_cli_config() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp_path).unwrap();

    let args = vec!["changepacks".to_string(), "config".to_string()];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_publish_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_publish_with_echo() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    // Create config with echo publish command
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join(".changepacks/config.json"),
        r#"{"publish": {"node": "echo test"}}"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_publish_no_projects() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    tokio::fs::write(temp_path.join("README.md"), "# Test")
        .await
        .unwrap();

    git_add_and_commit(temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_publish_json_format() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_update_with_changepack() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    init_git_repo(temp_path);

    // Create changepacks directory and update log
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
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

    git_add_and_commit(temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    // Canonicalize the path to avoid Windows path issues
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    // Create .changepacks directory (required by gen_update_map)
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec!["changepacks".to_string(), "check".to_string()];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok(), "check basic failed: {:?}", result.err());
}

#[tokio::test]
#[serial]
async fn test_cli_check_json_format() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "check json format failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_tree() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create multiple packages with workspace:* dependencies
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root-pkg", "version": "1.0.0", "dependencies": {"child-pkg": "workspace:*"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(temp_path.join("packages/child"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/child/package.json"),
        r#"{"name": "child-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(result.is_ok(), "check tree failed: {:?}", result.err());
}

#[tokio::test]
#[serial]
async fn test_cli_check_filter_package() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--filter".to_string(),
        "package".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "check filter package failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_filter_workspace() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create a pnpm workspace
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-workspace", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--filter".to_string(),
        "workspace".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "check filter workspace failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_with_changepack_updates() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    // Create changepacks directory and update log
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join(".changepacks/changepack_log_test.json"),
        r#"{"changes": {"package.json": "Minor"}, "note": "test feature", "date": "2025-01-01T00:00:00Z"}"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec!["changepacks".to_string(), "check".to_string()];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "check with changepack updates failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_check_no_projects() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(temp_path.join("README.md"), "# Test")
        .await
        .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec!["changepacks".to_string(), "check".to_string()];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "check no projects failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_changepacks_with_yes_and_message() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

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

    std::env::set_current_dir(&original_dir).unwrap();

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

#[tokio::test]
#[serial]
async fn test_cli_changepacks_no_projects() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(temp_path.join("README.md"), "# Test")
        .await
        .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

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

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

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

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "changepacks empty notes failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_cli_changepacks_with_filter() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create a pnpm workspace
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-workspace", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

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

    std::env::set_current_dir(&original_dir).unwrap();

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

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec!["changepacks".to_string(), "init".to_string()];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    // Should fail because already initialized
    assert!(result.is_err());
}

// Test publish with language filter
#[tokio::test]
#[serial]
async fn test_cli_publish_with_language_filter() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create Node.js package
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    // Create Rust package (should be filtered out)
    tokio::fs::write(
        temp_path.join("Cargo.toml"),
        r#"[package]
name = "test-rust"
version = "1.0.0"
"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    // Only publish Node.js packages
    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
        "--language".to_string(),
        "node".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(temp_path.join("packages/core"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/core/package.json"),
        r#"{"name": "core-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    // Only publish specific project
    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
        "--project".to_string(),
        "package.json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
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

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--dry-run".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "update JSON format failed: {:?}",
        result.err()
    );
}

// Test update with no updates found
#[tokio::test]
#[serial]
async fn test_cli_update_no_updates() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(temp_path.join("index.js"), "console.log('hello');")
        .await
        .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    // Modify the file to make the project "changed"
    tokio::fs::write(temp_path.join("index.js"), "console.log('modified');")
        .await
        .unwrap();

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec!["changepacks".to_string(), "check".to_string()];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create a complex dependency structure with workspace:* dependencies
    // root -> pkg-a, pkg-b
    // pkg-a -> pkg-c
    // pkg-b -> pkg-c (diamond pattern)
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*", "pkg-b": "workspace:*"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    for pkg in &["pkg-a", "pkg-b", "pkg-c"] {
        tokio::fs::create_dir_all(temp_path.join(format!("packages/{}", pkg)))
            .await
            .unwrap();
    }

    tokio::fs::write(
        temp_path.join("packages/pkg-a/package.json"),
        r#"{"name": "pkg-a", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        temp_path.join("packages/pkg-b/package.json"),
        r#"{"name": "pkg-b", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        temp_path.join("packages/pkg-c/package.json"),
        r#"{"name": "pkg-c", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    // Create config with echo publish command
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join(".changepacks/config.json"),
        r#"{"publish": {"node": "echo publishing"}}"#,
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

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join(".changepacks/changepack_log_test.json"),
        r#"{"changes": {"package.json": "Patch"}, "note": "test update", "date": "2025-01-01T00:00:00Z"}"#,
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

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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

// Test update with workspace dependencies
#[tokio::test]
#[serial]
async fn test_cli_update_with_workspace_deps() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create pnpm workspace
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    // Create core package
    tokio::fs::create_dir_all(temp_path.join("packages/core"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/core/package.json"),
        r#"{"name": "core", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    // Create cli package that depends on core via workspace:*
    tokio::fs::create_dir_all(temp_path.join("packages/cli"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/cli/package.json"),
        r#"{"name": "cli", "version": "1.0.0", "dependencies": {"core": "workspace:*"}}"#,
    )
    .await
    .unwrap();

    // Create changepack log for core only
    tokio::fs::write(
        temp_path.join(".changepacks/changepack_log_core.json"),
        r#"{"changes": {"packages/core/package.json": "Minor"}, "note": "update core", "date": "2025-01-01T00:00:00Z"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create changepack log for one package
    tokio::fs::write(
        temp_path.join(".changepacks/changepack_log_update.json"),
        r#"{"changes": {"packages/pkg-a/package.json": "Minor"}, "note": "update pkg-a", "date": "2025-01-01T00:00:00Z"}"#,
    )
    .await
    .unwrap();

    // Create packages with workspace:* dependencies
    // root -> pkg-a, pkg-b
    // pkg-a -> pkg-c
    // pkg-b -> pkg-c (diamond pattern)
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*", "pkg-b": "workspace:*"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    for pkg in &["pkg-a", "pkg-b", "pkg-c"] {
        tokio::fs::create_dir_all(temp_path.join(format!("packages/{}", pkg)))
            .await
            .unwrap();
    }

    tokio::fs::write(
        temp_path.join("packages/pkg-a/package.json"),
        r#"{"name": "pkg-a", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(temp_path.join("packages/pkg-a/index.js"), "// initial")
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("packages/pkg-b/package.json"),
        r#"{"name": "pkg-b", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        temp_path.join("packages/pkg-c/package.json"),
        r#"{"name": "pkg-c", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    // Modify pkg-a to make it "changed"
    tokio::fs::write(temp_path.join("packages/pkg-a/index.js"), "// modified")
        .await
        .unwrap();

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create packages - one with workspace:* deps, one orphaned
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root", "version": "1.0.0", "dependencies": {"child": "workspace:*"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(temp_path.join("packages/child"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/child/package.json"),
        r#"{"name": "child", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    // Create an orphaned package (not in any dependency chain)
    tokio::fs::create_dir_all(temp_path.join("packages/orphan"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/orphan/package.json"),
        r#"{"name": "orphan", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "check tree with orphan failed: {:?}",
        result.err()
    );
}

// Test publish with failing command (to cover error path)
#[tokio::test]
#[serial]
async fn test_cli_publish_with_failing_command() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    // Create config with failing publish command
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    let fail_cmd = if cfg!(target_os = "windows") {
        r#"{"publish": {"node": "cmd /c exit 1"}}"#
    } else {
        r#"{"publish": {"node": "exit 1"}}"#
    };
    tokio::fs::write(temp_path.join(".changepacks/config.json"), fail_cmd)
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create circular dependency: pkg-a -> pkg-b, pkg-b -> pkg-a
    // Neither is a root (both are in has_dependencies), so both become orphans
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(temp_path.join("packages/pkg-a"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/pkg-a/package.json"),
        r#"{"name": "pkg-a", "version": "1.0.0", "dependencies": {"pkg-b": "workspace:*"}}"#,
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(temp_path.join("packages/pkg-b"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/pkg-b/package.json"),
        r#"{"name": "pkg-b", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*"}}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(temp_path.join("README.md"), "# Test")
        .await
        .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "publish json no projects failed: {:?}",
        result.err()
    );
}

// Test check tree with workspace (covers check.rs lines 296, 303, 305-311)
#[tokio::test]
#[serial]
async fn test_cli_check_tree_with_workspace() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create a pnpm workspace with workspace:* dependencies
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root-workspace", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(temp_path.join("packages/pkg-a"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/pkg-a/package.json"),
        r#"{"name": "pkg-a", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "check tree with workspace failed: {:?}",
        result.err()
    );
}

// Test check tree with deeply nested dependencies (covers check.rs lines 216-250)
#[tokio::test]
#[serial]
async fn test_cli_check_tree_deeply_nested() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create a deep dependency chain with workspace:* deps: root -> a -> b -> c -> d
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root", "version": "1.0.0", "dependencies": {"pkg-a": "workspace:*"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    for (pkg, deps) in &[
        (
            "pkg-a",
            r#"{"name": "pkg-a", "version": "1.0.0", "dependencies": {"pkg-b": "workspace:*"}}"#,
        ),
        (
            "pkg-b",
            r#"{"name": "pkg-b", "version": "1.0.0", "dependencies": {"pkg-c": "workspace:*"}}"#,
        ),
        (
            "pkg-c",
            r#"{"name": "pkg-c", "version": "1.0.0", "dependencies": {"pkg-d": "workspace:*"}}"#,
        ),
        ("pkg-d", r#"{"name": "pkg-d", "version": "1.0.0"}"#),
    ] {
        tokio::fs::create_dir_all(temp_path.join(format!("packages/{}", pkg)))
            .await
            .unwrap();
        tokio::fs::write(
            temp_path.join(format!("packages/{}/package.json", pkg)),
            deps,
        )
        .await
        .unwrap();
    }

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create packages where shared-dep is depended on by multiple packages
    // root1 -> shared-dep (visits shared-dep first)
    // root2 -> [shared-dep, z-pkg] (shared-dep is NOT last after sorting, hits line 240)
    // Both root1 and root2 are root nodes
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root1", "version": "1.0.0", "dependencies": {"shared-dep": "workspace:*"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    for (pkg, deps) in &[
        // root2 depends on both shared-dep and z-pkg. After sorting: [shared-dep, z-pkg]
        // shared-dep is idx=0 (not last), so when already visited, hits line 240 (├──)
        (
            "root2",
            r#"{"name": "root2", "version": "1.0.0", "dependencies": {"shared-dep": "workspace:*", "z-pkg": "workspace:*"}}"#,
        ),
        (
            "shared-dep",
            r#"{"name": "shared-dep", "version": "1.0.0"}"#,
        ),
        ("z-pkg", r#"{"name": "z-pkg", "version": "1.0.0"}"#),
    ] {
        tokio::fs::create_dir_all(temp_path.join(format!("packages/{}", pkg)))
            .await
            .unwrap();
        tokio::fs::write(
            temp_path.join(format!("packages/{}/package.json", pkg)),
            deps,
        )
        .await
        .unwrap();
    }

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "check".to_string(),
        "--tree".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create a workspace and a package
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root-workspace", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(temp_path.join("packages/pkg"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join("packages/pkg/package.json"),
        r#"{"name": "pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

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

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "changepacks with package filter failed: {:?}",
        result.err()
    );
}

// Test publish dry-run with JSON format (covers publish.rs lines 102-103)
#[tokio::test]
#[serial]
async fn test_cli_publish_dry_run_json() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--dry-run".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "publish dry-run json failed: {:?}",
        result.err()
    );
}

// Test update dry-run with JSON format (covers update.rs lines 102-103)
#[tokio::test]
#[serial]
async fn test_cli_update_dry_run_json() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
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

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--dry-run".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "update dry-run json failed: {:?}",
        result.err()
    );
}

// Test publish stdout with actual execution (covers publish.rs lines 131-139)
#[tokio::test]
#[serial]
async fn test_cli_publish_stdout_execution() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    // Create config with echo publish command
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    tokio::fs::write(
        temp_path.join(".changepacks/config.json"),
        r#"{"publish": {"node": "echo publishing stdout"}}"#,
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

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

    assert!(
        result.is_ok(),
        "publish stdout execution failed: {:?}",
        result.err()
    );
}

// Test update dry-run with stdout format (covers update.rs lines 99-100)
#[tokio::test]
#[serial]
async fn test_cli_update_dry_run_stdout() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
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

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    // Use default stdout format with dry-run (not JSON)
    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--dry-run".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    // Create a pnpm workspace
    tokio::fs::write(
        temp_path.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*",
    )
    .await
    .unwrap();
    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "root-workspace", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    // Create changepack log for the workspace
    tokio::fs::write(
        temp_path.join(".changepacks/changepack_log_ws.json"),
        r#"{"changes": {"package.json": "Minor"}, "note": "update workspace", "date": "2025-01-01T00:00:00Z"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    let args = vec![
        "changepacks".to_string(),
        "update".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test-pkg", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    // Run without --update-type, so it will iterate Major, Minor, Patch
    let args = vec![
        "changepacks".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        "Test without update type".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().canonicalize().unwrap();

    init_git_repo(&temp_path);

    // Create config with failing publish command
    tokio::fs::create_dir_all(temp_path.join(".changepacks"))
        .await
        .unwrap();
    let fail_cmd = if cfg!(target_os = "windows") {
        r#"{"publish": {"node": "cmd /c exit 1"}}"#
    } else {
        r#"{"publish": {"node": "exit 1"}}"#
    };
    tokio::fs::write(temp_path.join(".changepacks/config.json"), fail_cmd)
        .await
        .unwrap();

    tokio::fs::write(
        temp_path.join("package.json"),
        r#"{"name": "test", "version": "1.0.0"}"#,
    )
    .await
    .unwrap();

    git_add_and_commit(&temp_path, "Initial commit");

    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&temp_path).unwrap();

    // Use stdout format (default) to hit the error eprintln! path
    let args = vec![
        "changepacks".to_string(),
        "publish".to_string(),
        "--yes".to_string(),
    ];
    let result = changepacks_cli::main(&args).await;

    std::env::set_current_dir(&original_dir).unwrap();

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
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().canonicalize().unwrap();

        init_git_repo(&temp_path);

        tokio::fs::create_dir_all(temp_path.join(".changepacks"))
            .await
            .unwrap();

        tokio::fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(&temp_path, "Initial commit");

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_path).unwrap();

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

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_ok(), "publish cancelled should succeed");
    }

    // Test publish cancelled with JSON format (covers publish.rs lines 120-122)
    #[tokio::test]
    #[serial]
    async fn test_publish_cancelled_json() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().canonicalize().unwrap();

        init_git_repo(&temp_path);

        tokio::fs::create_dir_all(temp_path.join(".changepacks"))
            .await
            .unwrap();

        tokio::fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(&temp_path, "Initial commit");

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_path).unwrap();

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

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_ok(), "publish cancelled json should succeed");
    }

    // Test update cancelled (covers update.rs lines 115-123)
    #[tokio::test]
    #[serial]
    async fn test_update_cancelled_stdout() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().canonicalize().unwrap();

        init_git_repo(&temp_path);

        tokio::fs::create_dir_all(temp_path.join(".changepacks"))
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

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_path).unwrap();

        let args = UpdateArgs {
            dry_run: false,
            yes: false,
            format: FormatOptions::Stdout,
            remote: false,
        };

        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };

        let result = handle_update_with_prompter(&args, &prompter).await;

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_ok(), "update cancelled should succeed");
    }

    // Test update cancelled with JSON format (covers update.rs lines 119-121)
    #[tokio::test]
    #[serial]
    async fn test_update_cancelled_json() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().canonicalize().unwrap();

        init_git_repo(&temp_path);

        tokio::fs::create_dir_all(temp_path.join(".changepacks"))
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

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_path).unwrap();

        let args = UpdateArgs {
            dry_run: false,
            yes: false,
            format: FormatOptions::Json,
            remote: false,
        };

        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };

        let result = handle_update_with_prompter(&args, &prompter).await;

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_ok(), "update cancelled json should succeed");
    }

    // Test changepacks with interactive selection (covers changepacks.rs lines 61-95)
    #[tokio::test]
    #[serial]
    async fn test_changepacks_interactive_select() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().canonicalize().unwrap();

        init_git_repo(&temp_path);

        tokio::fs::create_dir_all(temp_path.join(".changepacks"))
            .await
            .unwrap();

        tokio::fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(&temp_path, "Initial commit");

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_path).unwrap();

        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: false,                                // Use interactive mode
            message: Some("test message".to_string()), // Provide message to skip text prompt
            update_type: None,                         // Will iterate through Major, Minor, Patch
        };

        let prompter = MockPrompter {
            select_all: true,
            confirm_value: true,
            text_value: "test note".to_string(),
        };

        let result = handle_changepack_with_prompter(&args, &prompter).await;

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_ok(), "changepacks interactive should succeed");
    }

    // Test changepacks with no selection (covers changepacks.rs empty selection path)
    #[tokio::test]
    #[serial]
    async fn test_changepacks_no_selection() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().canonicalize().unwrap();

        init_git_repo(&temp_path);

        tokio::fs::create_dir_all(temp_path.join(".changepacks"))
            .await
            .unwrap();

        tokio::fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(&temp_path, "Initial commit");

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_path).unwrap();

        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: false,
            message: Some("test".to_string()),
            update_type: None,
        };

        let prompter = MockPrompter {
            select_all: false, // Select nothing
            confirm_value: true,
            text_value: "test note".to_string(),
        };

        let result = handle_changepack_with_prompter(&args, &prompter).await;

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_ok(), "changepacks no selection should succeed");
    }

    // Test changepacks with text prompt (covers changepacks.rs line 133)
    #[tokio::test]
    #[serial]
    async fn test_changepacks_text_prompt() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().canonicalize().unwrap();

        init_git_repo(&temp_path);

        tokio::fs::create_dir_all(temp_path.join(".changepacks"))
            .await
            .unwrap();

        tokio::fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();

        git_add_and_commit(&temp_path, "Initial commit");

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_path).unwrap();

        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: true,     // Auto-select all
            message: None, // No message, will use text prompt
            update_type: Some(changepacks_core::UpdateType::Patch),
        };

        let prompter = MockPrompter {
            select_all: true,
            confirm_value: true,
            text_value: "prompted note".to_string(),
        };

        let result = handle_changepack_with_prompter(&args, &prompter).await;

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_ok(), "changepacks text prompt should succeed");
    }

    // Test changepacks with changed project in interactive mode (covers changepacks.rs line 77)
    // Line 77 is `Some(index)` when project.is_changed() returns true
    #[tokio::test]
    #[serial]
    async fn test_changepacks_interactive_with_changed_project() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().canonicalize().unwrap();

        init_git_repo(&temp_path);

        tokio::fs::create_dir_all(temp_path.join(".changepacks"))
            .await
            .unwrap();

        tokio::fs::write(
            temp_path.join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(temp_path.join("index.js"), "// initial")
            .await
            .unwrap();

        git_add_and_commit(&temp_path, "Initial commit");

        // Modify a file to make the project "changed"
        tokio::fs::write(temp_path.join("index.js"), "// modified")
            .await
            .unwrap();

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_path).unwrap();

        // Use interactive mode with update_type: None (will iterate Major, Minor, Patch)
        // The changed project should be detected and line 77 will be hit
        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: false, // Interactive mode
            message: Some("test message".to_string()),
            update_type: None, // Will iterate through all update types
        };

        let prompter = MockPrompter {
            select_all: true,
            confirm_value: true,
            text_value: "test note".to_string(),
        };

        let result = handle_changepack_with_prompter(&args, &prompter).await;

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(
            result.is_ok(),
            "changepacks with changed project should succeed"
        );
    }
}
