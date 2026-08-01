use changepacks_core::{Project, ProjectFinder};
use changepacks_csharp::CSharpProjectFinder;
use changepacks_dart::DartProjectFinder;
use changepacks_java::GradleProjectFinder;
use changepacks_node::NodeProjectFinder;
use changepacks_python::PythonProjectFinder;
use changepacks_rust::RustProjectFinder;

/// Get finder list
#[must_use]
pub(crate) fn get_finders() -> Vec<Box<dyn ProjectFinder>> {
    vec![
        Box::new(NodeProjectFinder::new()),
        Box::new(RustProjectFinder::new()),
        Box::new(PythonProjectFinder::new()),
        Box::new(DartProjectFinder::new()),
        Box::new(CSharpProjectFinder::new()),
        Box::new(GradleProjectFinder::new()),
    ]
}

/// Calculate total project count across all finders for capacity hints.
///
/// Private to this module: it is a capacity hint for [`collect_projects`] and
/// [`collect_projects_mut`], never a standalone API. Callers outside the module
/// go through those collectors instead.
#[must_use]
fn total_project_count(finders: &[Box<dyn ProjectFinder>]) -> usize {
    finders.iter().map(|f| f.project_count()).sum()
}

/// Collect all projects from finders into a single Vec with pre-allocated capacity.
///
/// Uses [`ProjectFinder::extend_projects`] rather than `flat_map(|f| f.projects())`
/// so each finder appends straight into the pre-sized result buffer. The
/// `projects()` shape allocated — and immediately dropped — one intermediate
/// `Vec<&Project>` per finder (six of them, one per language) on every
/// `check`, `update`, `publish`, and default-changepack run. Order is
/// unchanged: finders are still walked in `get_finders()` order and each one
/// still appends in its own `projects()` order.
#[must_use]
pub(crate) fn collect_projects(finders: &[Box<dyn ProjectFinder>]) -> Vec<&Project> {
    let cap = total_project_count(finders);
    let mut projects = Vec::with_capacity(cap);
    for finder in finders {
        finder.extend_projects(&mut projects);
    }
    projects
}

/// Mutable counterpart of [`collect_projects`]: collect every project from
/// `finders` as `&mut Project` into a single pre-allocated Vec.
///
/// Drives [`ProjectFinder::extend_projects_mut`] for the same reason
/// [`collect_projects`] drives `extend_projects`: `flat_map(|f| f.projects_mut())`
/// (or a nested `for project in finder.projects_mut()` loop) allocated — and
/// immediately dropped — one intermediate `Vec<&mut Project>` per finder, six
/// of them per `changepacks update` run. Order is identical to the nested-loop
/// shape: finders are walked in `get_finders()` order and each one appends in
/// its own `projects_mut()` order.
#[must_use]
pub(crate) fn collect_projects_mut(finders: &mut [Box<dyn ProjectFinder>]) -> Vec<&mut Project> {
    let cap = total_project_count(finders);
    let mut projects = Vec::with_capacity(cap);
    for finder in finders.iter_mut() {
        finder.extend_projects_mut(&mut projects);
    }
    projects
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// Seed `root` with one Node manifest and one Rust manifest, drive the two
    /// matching finders over them, and hand back both finders in the relative
    /// order [`get_finders`] uses (Node before Rust).
    ///
    /// Every other test in this module builds finders straight from
    /// [`get_finders`], which have discovered nothing, so the `extend_projects`
    /// loop bodies in [`collect_projects`] / [`collect_projects_mut`] never run
    /// with a non-empty finder and the documented merge ORDER is untested. This
    /// fixture is what makes those loop bodies observable.
    ///
    /// The directory names are deliberately reverse-alphabetical to the finder
    /// order (`z_node` sorts AFTER `a_rust`), so an assertion on the emitted
    /// order can only hold by honouring the finder walk order — it can never be
    /// satisfied by an accidental sort on path.
    async fn discovered_node_then_rust(root: &Path) -> Vec<Box<dyn ProjectFinder>> {
        let node_dir = root.join("z_node");
        let rust_dir = root.join("a_rust");
        fs::create_dir(&node_dir).unwrap();
        fs::create_dir(&rust_dir).unwrap();

        let package_json = node_dir.join("package.json");
        fs::write(&package_json, r#"{"name":"node-pkg","version":"1.0.0"}"#).unwrap();
        let cargo_toml = rust_dir.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            "[package]\nname = \"rust-pkg\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        let mut node_finder = NodeProjectFinder::new();
        node_finder
            .visit(&package_json, Path::new("z_node/package.json"))
            .await
            .unwrap();
        let mut rust_finder = RustProjectFinder::new();
        rust_finder
            .visit(&cargo_toml, Path::new("a_rust/Cargo.toml"))
            .await
            .unwrap();

        assert_eq!(node_finder.project_count(), 1);
        assert_eq!(rust_finder.project_count(), 1);

        vec![Box::new(node_finder), Box::new(rust_finder)]
    }

    /// With finders that actually hold projects, `collect_projects` must emit
    /// every project exactly once, size the buffer from `total_project_count`,
    /// and preserve `get_finders()` order across finder boundaries.
    #[tokio::test]
    async fn test_collect_projects_preserves_finder_order() {
        let temp_dir = TempDir::new().unwrap();
        let finders = discovered_node_then_rust(temp_dir.path()).await;

        let projects = collect_projects(&finders);

        assert_eq!(projects.len(), 2);
        assert_eq!(projects.len(), total_project_count(&finders));
        assert_eq!(
            projects[0].relative_path(),
            Path::new("z_node/package.json"),
            "the Node finder comes first in get_finders(), so its project must be emitted first"
        );
        assert_eq!(projects[1].relative_path(), Path::new("a_rust/Cargo.toml"));

        temp_dir.close().unwrap();
    }

    /// `collect_projects_mut` must agree with `collect_projects` on the merged
    /// order and on the count, not just on the empty-finder degenerate case.
    #[tokio::test]
    async fn test_collect_projects_mut_preserves_finder_order() {
        let temp_dir = TempDir::new().unwrap();
        let mut finders = discovered_node_then_rust(temp_dir.path()).await;
        let expected = total_project_count(&finders);

        let projects = collect_projects_mut(&mut finders);

        assert_eq!(projects.len(), 2);
        assert_eq!(projects.len(), expected);
        assert_eq!(
            projects[0].relative_path(),
            Path::new("z_node/package.json"),
            "the mutable collector must merge in the same order as collect_projects"
        );
        assert_eq!(projects[1].relative_path(), Path::new("a_rust/Cargo.toml"));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_finders() {
        let finders = get_finders();
        assert_eq!(finders.len(), 6);
    }

    #[test]
    fn test_total_project_count() {
        let finders = get_finders();
        let count = total_project_count(&finders);
        // Empty finders (no projects discovered yet) should sum to 0
        assert_eq!(count, 0);
    }

    // `collect_projects` now drives `ProjectFinder::extend_projects` instead of
    // `flat_map(|f| f.projects())`. The seeded capacity must still describe the
    // result exactly, and undiscovered finders must contribute nothing.
    #[test]
    fn test_collect_projects_matches_total_project_count() {
        let finders = get_finders();
        let projects = collect_projects(&finders);
        assert_eq!(projects.len(), total_project_count(&finders));
        assert!(projects.is_empty());
    }

    // `collect_projects_mut` must agree with `collect_projects` on both the
    // seeded capacity and the emitted order; undiscovered finders contribute
    // nothing to either.
    #[test]
    fn test_collect_projects_mut_matches_total_project_count() {
        let mut finders = get_finders();
        let expected = total_project_count(&finders);
        let projects = collect_projects_mut(&mut finders);
        assert_eq!(projects.len(), expected);
        assert!(projects.is_empty());
    }
}
