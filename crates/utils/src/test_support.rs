//! Shared `#[cfg(test)]` fixtures for this crate's unit tests.
//!
//! Declared `#[cfg(test)]` in `lib.rs`, so nothing here is compiled into a
//! non-test build. Relies on the `changepacks-node` dev-dependency that every
//! consuming test module already imports.

use std::path::PathBuf;

use changepacks_core::{Package, Project};
use changepacks_node::package::NodePackage;

/// Create a test project (a `NodePackage` at version `1.0.0`) with the given
/// dependencies.
///
/// Dependencies are stored as *names* (e.g. `"p2"`), matching every real
/// finder — `add_dependency` records the raw name, never a relative path — so
/// `sort_by_dependencies` resolves them by name via `name_to_index` and
/// `apply_reverse_dependencies` resolves them via `reverse_deps`.
pub(crate) fn create_project(name: &str, dependencies: Vec<&str>) -> Project {
    let mut package = NodePackage::new(
        Some(name.to_string()),
        Some("1.0.0".to_string()),
        PathBuf::from(format!("/test/{}/package.json", name)),
        PathBuf::from(format!("{}/package.json", name)),
    );
    for dep in dependencies {
        package.add_dependency(dep);
    }
    Project::Package(Box::new(package))
}
