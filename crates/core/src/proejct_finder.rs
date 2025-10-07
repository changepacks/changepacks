use crate::project::Project;

pub trait ProjectFinder {
    fn new(root: Option<String>) -> Self;
    fn find(&self) -> Vec<Project>;
}
