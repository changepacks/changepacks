pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::NodeProjectFinder;

use std::path::Path;

/// Represents the detected Node.js package manager
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Bun,
}

impl PackageManager {
    /// Returns the publish command for this package manager
    pub fn publish_command(&self) -> &'static str {
        match self {
            Self::Npm => "npm publish",
            Self::Yarn => "yarn npm publish",
            Self::Pnpm => "pnpm publish",
            Self::Bun => "bun publish",
        }
    }
}

/// Detects the package manager by checking for lock files in the given directory
/// Priority: bun.lockb > pnpm-lock.yaml > yarn.lock > package-lock.json > npm (default)
pub fn detect_package_manager(dir: &Path) -> PackageManager {
    if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        PackageManager::Bun
    } else if dir.join("pnpm-lock.yaml").exists() {
        PackageManager::Pnpm
    } else if dir.join("yarn.lock").exists() {
        PackageManager::Yarn
    } else if dir.join("package-lock.json").exists() {
        PackageManager::Npm
    } else {
        // Default to npm if no lock file found
        PackageManager::Npm
    }
}

/// Detects the package manager by searching from the given path up to the root
pub fn detect_package_manager_recursive(path: &Path) -> PackageManager {
    let mut current = if path.is_file() {
        path.parent()
    } else {
        Some(path)
    };

    while let Some(dir) = current {
        let pm = detect_package_manager(dir);
        if pm != PackageManager::Npm || dir.join("package-lock.json").exists() {
            return pm;
        }
        current = dir.parent();
    }

    PackageManager::Npm
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_detect_bun_lockb() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("bun.lockb"), "").unwrap();
        assert_eq!(detect_package_manager(temp_dir.path()), PackageManager::Bun);
    }

    #[test]
    fn test_detect_bun_lock() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("bun.lock"), "").unwrap();
        assert_eq!(detect_package_manager(temp_dir.path()), PackageManager::Bun);
    }

    #[test]
    fn test_detect_pnpm() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(
            detect_package_manager(temp_dir.path()),
            PackageManager::Pnpm
        );
    }

    #[test]
    fn test_detect_yarn() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(
            detect_package_manager(temp_dir.path()),
            PackageManager::Yarn
        );
    }

    #[test]
    fn test_detect_npm() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("package-lock.json"), "{}").unwrap();
        assert_eq!(detect_package_manager(temp_dir.path()), PackageManager::Npm);
    }

    #[test]
    fn test_detect_default_npm() {
        let temp_dir = TempDir::new().unwrap();
        assert_eq!(detect_package_manager(temp_dir.path()), PackageManager::Npm);
    }

    #[test]
    fn test_bun_priority_over_others() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("bun.lockb"), "").unwrap();
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        fs::write(temp_dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(detect_package_manager(temp_dir.path()), PackageManager::Bun);
    }

    #[test]
    fn test_pnpm_priority_over_yarn() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        fs::write(temp_dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(
            detect_package_manager(temp_dir.path()),
            PackageManager::Pnpm
        );
    }

    #[test]
    fn test_publish_commands() {
        assert_eq!(PackageManager::Npm.publish_command(), "npm publish");
        assert_eq!(PackageManager::Yarn.publish_command(), "yarn npm publish");
        assert_eq!(PackageManager::Pnpm.publish_command(), "pnpm publish");
        assert_eq!(PackageManager::Bun.publish_command(), "bun publish");
    }

    #[test]
    fn test_detect_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let sub_dir = temp_dir.path().join("packages").join("core");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(temp_dir.path().join("pnpm-lock.yaml"), "").unwrap();
        fs::write(sub_dir.join("package.json"), "{}").unwrap();

        assert_eq!(
            detect_package_manager_recursive(&sub_dir.join("package.json")),
            PackageManager::Pnpm
        );
    }
}
