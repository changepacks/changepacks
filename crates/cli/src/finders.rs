use changepacks_core::{Project, ProjectFinder};
use changepacks_csharp::CSharpProjectFinder;
use changepacks_dart::DartProjectFinder;
use changepacks_java::GradleProjectFinder;
use changepacks_node::NodeProjectFinder;
use changepacks_python::PythonProjectFinder;
use changepacks_rust::RustProjectFinder;

/// Get finder list
#[must_use]
pub fn get_finders() -> Vec<Box<dyn ProjectFinder>> {
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
pub fn total_project_count(finders: &[Box<dyn ProjectFinder>]) -> usize {
    finders.iter().map(|f| f.project_count()).sum()
}

/// Collect all projects from finders into a single Vec with pre-allocated capacity.
#[must_use]
pub fn collect_projects(finders: &[Box<dyn ProjectFinder>]) -> Vec<&Project> {
    let cap = total_project_count(finders);
    let mut projects = Vec::with_capacity(cap);
    projects.extend(finders.iter().flat_map(|finder| finder.projects()));
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
}
