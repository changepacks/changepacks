use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Loaded from `.changepacks/config.json`, controls ignore patterns, base branch, publish commands, and update-on rules.
///
/// Configuration can specify custom publish commands per language or per project path,
/// ignore patterns using globs, and forced update rules for dependent packages.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Glob patterns for files/projects to ignore (e.g., "examples/**")
    #[serde(default)]
    pub ignore: Vec<String>,

    /// Base branch to compare against for change detection (default: "main")
    #[serde(default = "default_base_branch")]
    pub base_branch: String,

    /// Optional path to the default main package for versioning
    #[serde(default)]
    pub latest_package: Option<String>,

    /// Custom publish commands by language key or project path
    #[serde(default)]
    pub publish: HashMap<String, String>,

    /// Dependency rules for forced updates.
    /// Key: glob pattern for trigger packages (e.g., "crates/*")
    /// Value: list of package paths that must be updated when trigger matches
    #[serde(default)]
    pub update_on: HashMap<String, Vec<String>>,
}

fn default_base_branch() -> String {
    "main".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ignore: Vec::new(),
            base_branch: default_base_branch(),
            latest_package: None,
            publish: HashMap::new(),
            update_on: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.ignore.is_empty());
        assert_eq!(config.base_branch, "main");
        assert!(config.latest_package.is_none());
        assert!(config.publish.is_empty());
        assert!(config.update_on.is_empty());
    }

    #[test]
    fn test_config_deserialize_full() {
        let json = r#"{
            "ignore": ["examples/**", "docs/**"],
            "baseBranch": "develop",
            "latestPackage": "crates/core/Cargo.toml",
            "publish": {
                "node": "npm publish --access public",
                "rust": "cargo publish"
            },
            "updateOn": {
                "crates/core/Cargo.toml": ["bridge/node/package.json", "bridge/python/pyproject.toml"]
            }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.ignore, vec!["examples/**", "docs/**"]);
        assert_eq!(config.base_branch, "develop");
        assert_eq!(
            config.latest_package.as_deref(),
            Some("crates/core/Cargo.toml")
        );
        assert_eq!(config.publish.len(), 2);
        assert_eq!(
            config.publish.get("node").unwrap(),
            "npm publish --access public"
        );
        assert_eq!(config.publish.get("rust").unwrap(), "cargo publish");
        assert_eq!(config.update_on.len(), 1);
        let update_targets = config.update_on.get("crates/core/Cargo.toml").unwrap();
        assert_eq!(update_targets.len(), 2);
        assert!(update_targets.contains(&"bridge/node/package.json".to_string()));
        assert!(update_targets.contains(&"bridge/python/pyproject.toml".to_string()));
    }

    #[test]
    fn test_config_deserialize_partial() {
        let json = r#"{ "baseBranch": "release" }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.ignore.is_empty());
        assert_eq!(config.base_branch, "release");
        assert!(config.latest_package.is_none());
        assert!(config.publish.is_empty());
        assert!(config.update_on.is_empty());
    }

    #[test]
    fn test_config_deserialize_empty_object() {
        let json = r#"{}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.base_branch, "main");
        assert!(config.ignore.is_empty());
        assert!(config.latest_package.is_none());
        assert!(config.publish.is_empty());
        assert!(config.update_on.is_empty());
    }

    #[test]
    fn test_config_ignore_patterns() {
        let json = r#"{ "ignore": ["**/*", "!crates/changepacks/Cargo.toml", "!bridge/**"] }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.ignore.len(), 3);
        assert_eq!(config.ignore[0], "**/*");
        assert_eq!(config.ignore[1], "!crates/changepacks/Cargo.toml");
        assert_eq!(config.ignore[2], "!bridge/**");
    }

    #[test]
    fn test_config_publish_map() {
        let json = r#"{
            "publish": {
                "node": "npm publish",
                "python": "uv publish",
                "rust": "cargo publish",
                "dart": "dart pub publish",
                "bridge/node/package.json": "npm publish --access public"
            }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.publish.len(), 5);
        assert_eq!(config.publish.get("node").unwrap(), "npm publish");
        assert_eq!(config.publish.get("python").unwrap(), "uv publish");
        assert_eq!(config.publish.get("rust").unwrap(), "cargo publish");
        assert_eq!(config.publish.get("dart").unwrap(), "dart pub publish");
        assert_eq!(
            config.publish.get("bridge/node/package.json").unwrap(),
            "npm publish --access public"
        );
    }

    #[test]
    fn test_config_update_on_map() {
        let json = r#"{
            "updateOn": {
                "crates/changepacks/Cargo.toml": ["bridge/node/package.json"],
                "crates/core/Cargo.toml": ["bridge/python/pyproject.toml", "bridge/node/package.json"]
            }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.update_on.len(), 2);

        let changepacks_targets = config
            .update_on
            .get("crates/changepacks/Cargo.toml")
            .unwrap();
        assert_eq!(changepacks_targets.len(), 1);
        assert_eq!(changepacks_targets[0], "bridge/node/package.json");

        let core_targets = config.update_on.get("crates/core/Cargo.toml").unwrap();
        assert_eq!(core_targets.len(), 2);
    }

    #[test]
    fn test_config_serialize_roundtrip() {
        let mut config = Config {
            ignore: vec!["test/**".to_string()],
            base_branch: "develop".to_string(),
            latest_package: Some("Cargo.toml".to_string()),
            ..Config::default()
        };
        config
            .publish
            .insert("rust".to_string(), "cargo publish".to_string());
        config.update_on.insert(
            "Cargo.toml".to_string(),
            vec!["bridge/package.json".to_string()],
        );

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }
}
