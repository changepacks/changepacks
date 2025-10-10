use core::ProjectFinder;
use node::NodeProjectFinder;
use python::PythonProjectFinder;
use rust::RustProjectFinder;

// finder list

pub fn get_finders() -> [Box<dyn ProjectFinder>; 3] {
    [
        Box::new(NodeProjectFinder::new()),
        Box::new(RustProjectFinder::new()),
        Box::new(PythonProjectFinder::new()),
    ]
}
