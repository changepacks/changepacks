use changepack_core::ProjectFinder;
use dart::DartProjectFinder;
use node::NodeProjectFinder;
use python::PythonProjectFinder;
use rust::RustProjectFinder;

/// Get finder list
pub fn get_finders() -> Vec<Box<dyn ProjectFinder>>
{
    vec![
        Box::new(NodeProjectFinder::new()),
        Box::new(RustProjectFinder::new()),
        Box::new(PythonProjectFinder::new()),
        Box::new(DartProjectFinder::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_finders() {
        let finders = get_finders();
        assert_eq!(finders.len(), 4);
    }
}
