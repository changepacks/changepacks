use changepack_core::ProjectFinder;
use node::NodeProjectFinder;
use python::PythonProjectFinder;
use rust::RustProjectFinder;

/// Get finder list
pub fn get_finders() -> [Box<dyn ProjectFinder>; 3] {
    [
        Box::new(NodeProjectFinder::new()),
        Box::new(RustProjectFinder::new()),
        Box::new(PythonProjectFinder::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_finders() {
        let finders = get_finders();
        assert_eq!(finders.len(), 3);
    }
}
