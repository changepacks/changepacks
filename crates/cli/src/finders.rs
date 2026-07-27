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
#[must_use]
pub(crate) fn total_project_count(finders: &[Box<dyn ProjectFinder>]) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
