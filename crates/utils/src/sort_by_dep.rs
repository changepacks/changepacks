use changepacks_core::Project;
use std::collections::{HashMap, VecDeque};

/// Sort projects by their dependencies using topological sort.
/// Projects with no dependencies or whose dependencies are already published will come first.
/// Returns a sorted vector of project references (no cloning, just reordering).
#[must_use]
pub fn sort_by_dependencies(projects: Vec<&Project>) -> Vec<&Project> {
    // For 0 or 1 projects the topological ordering is provably identical to
    // the input slice, and Kahn's machinery would only allocate a HashMap,
    // two Vecs, a VecDeque, and two more Vecs to arrive at the same answer.
    // Widen the historical empty-check to skip that work on single-project
    // repos too (a common shape: monopackage crates with one Cargo.toml).
    if projects.len() <= 1 {
        return projects;
    }

    // Map from project name to index. Every finder that populates
    // dependencies stores them as *names* (JSON dep keys / `[tool.uv.sources]`
    // keys / Cargo `[dependencies]` workspace entries / pubspec dep keys /
    // `.csproj` filename stems), never as relative paths — so the historical
    // path-side lookup was dead code and its `to_string_lossy().into_owned()`
    // per project was pure allocation waste. Name lookup is sufficient.
    //
    // Keys are borrowed `&str` from `project.name()` — the input `projects`
    // slice lives for the whole function scope so borrowing is sound, and it
    // eliminates N `String` allocations (one per project) on every
    // `publish` / `check --tree` invocation.
    let mut name_to_index: HashMap<&str, usize> = HashMap::new();
    for (idx, project) in projects.iter().enumerate() {
        if let Some(name) = project.name() {
            name_to_index.insert(name, idx);
        }
    }

    // Build dependency graph: for each project, find which projects depend on it
    // in_degree[i] = number of dependencies that project i has
    let mut in_degree: Vec<usize> = vec![0; projects.len()];
    // graph[i] = list of projects that depend on project i
    let mut graph: Vec<Vec<usize>> = vec![Vec::new(); projects.len()];

    for (idx, project) in projects.iter().enumerate() {
        let deps = project.dependencies();
        for dep in deps {
            // `dep: &String` — call `.as_str()` so the borrow-key lookup on
            // `HashMap<&str, usize>` resolves without an intermediate
            // `String` alloc. (`.get(&String)` needs `&str: Borrow<String>`,
            // which is not implemented; the borrow direction is the reverse.)
            if let Some(&dep_idx) = name_to_index.get(dep.as_str()) {
                // Project at idx depends on project at dep_idx
                // So dep_idx should come before idx
                graph[dep_idx].push(idx);
                in_degree[idx] += 1;
            }
        }
    }

    // Kahn's algorithm for topological sort
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (idx, &degree) in in_degree.iter().enumerate() {
        if degree == 0 {
            queue.push_back(idx);
        }
    }

    // Kahn's traversal + the trailing cyclic-fallback loop below together
    // push every index in `0..projects.len()` at most once, so the final
    // length is bounded by `projects.len()`. Preallocating up front removes
    // the ~log2(N) geometric-doubling reallocations `Vec::new()` would
    // otherwise incur on every `publish` / `check --tree` invocation.
    let mut sorted_indices: Vec<usize> = Vec::with_capacity(projects.len());

    // Kahn's invariant: each node is pushed to the queue at most once, so the
    // per-pop membership guard is unreachable. The initial fill enumerates
    // each index exactly once, and inside the loop each edge is walked
    // exactly once (deps are stored in a `HashSet<String>` so no duplicate
    // edges, and `name_to_index.get(dep)` resolves each dep to a single
    // index), so every `in_degree` decrement is unique and the `== 0`
    // push happens at most once per node.
    // After the loop drains, `in_degree[idx] == 0` iff `idx` was popped and
    // its outgoing edges walked — i.e. it is on the acyclic frontier and
    // already in `sorted_indices`. Any `in_degree[idx] > 0` node is exactly
    // a cyclic node whose remaining dependency was never decremented to 0;
    // the trailing cyclic-fallback loop appends those. This makes the
    // dedicated `visited: Vec<bool>` redundant with `in_degree`, so we drop
    // its allocation + per-pop write and reuse Kahn's canonical marker.
    while let Some(idx) = queue.pop_front() {
        sorted_indices.push(idx);

        // Decrease in-degree of dependent projects
        for &dependent_idx in &graph[idx] {
            in_degree[dependent_idx] -= 1;
            if in_degree[dependent_idx] == 0 {
                queue.push_back(dependent_idx);
            }
        }
    }

    // Add any remaining projects that weren't part of the dependency graph.
    // Cyclic nodes are exactly those whose `in_degree` never reached 0.
    for (idx, &degree) in in_degree.iter().enumerate() {
        if degree > 0 {
            sorted_indices.push(idx);
        }
    }

    // Reorder projects based on sorted indices (no cloning, just reordering references)
    sorted_indices.iter().map(|&idx| projects[idx]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::{Package, Project};
    use changepacks_node::package::NodePackage;
    use std::path::PathBuf;

    // Helper function to create a test project with dependencies
    // Dependencies are stored as paths (e.g., "p2" -> "p2/package.json")
    fn create_project(name: &str, dependencies: Vec<&str>) -> Project {
        let mut package = NodePackage::new(
            Some(name.to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from(format!("/test/{}/package.json", name)),
            PathBuf::from(format!("{}/package.json", name)),
        );
        for dep in dependencies {
            // Store dependency as path (e.g., "p2" -> "p2/package.json")
            package.add_dependency(dep);
        }
        Project::Package(Box::new(package))
    }

    #[test]
    fn test_sort_empty() {
        let projects: Vec<&Project> = vec![];
        let sorted = sort_by_dependencies(projects);
        assert_eq!(sorted.len(), 0);
    }

    #[test]
    fn test_sort_no_dependencies() {
        let p1 = create_project("p1", vec![]);
        let p2 = create_project("p2", vec![]);
        let p3 = create_project("p3", vec![]);

        let projects = vec![&p3, &p1, &p2];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 3);
        // All have no dependencies, so order should be preserved or stable
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();
        assert!(names.contains(&Some("p1")));
        assert!(names.contains(&Some("p2")));
        assert!(names.contains(&Some("p3")));
    }

    #[test]
    fn test_sort_simple_chain() {
        // p1 -> p2 -> p3 (p1 depends on p2, p2 depends on p3)
        let p3 = create_project("p3", vec![]);
        let p2 = create_project("p2", vec!["p3"]);
        let p1 = create_project("p1", vec!["p2"]);

        let projects = vec![&p1, &p2, &p3];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 3);
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();

        // p3 should come first (no dependencies)
        assert_eq!(names[0], Some("p3"));
        // p2 should come after p3
        assert_eq!(names[1], Some("p2"));
        // p1 should come last
        assert_eq!(names[2], Some("p1"));
    }

    #[test]
    fn test_sort_reverse_order() {
        // Same dependencies but input in reverse order
        let p3 = create_project("p3", vec![]);
        let p2 = create_project("p2", vec!["p3"]);
        let p1 = create_project("p1", vec!["p2"]);

        let projects = vec![&p3, &p2, &p1];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 3);
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();

        // Should still be sorted correctly: p3 -> p2 -> p1
        assert_eq!(names[0], Some("p3"));
        assert_eq!(names[1], Some("p2"));
        assert_eq!(names[2], Some("p1"));
    }

    #[test]
    fn test_sort_complex_graph() {
        // Complex dependency graph:
        // p1 -> p2, p3
        // p2 -> p4
        // p3 -> p4
        // p4 -> (no dependencies)
        let p4 = create_project("p4", vec![]);
        let p3 = create_project("p3", vec!["p4"]);
        let p2 = create_project("p2", vec!["p4"]);
        let p1 = create_project("p1", vec!["p2", "p3"]);

        let projects = vec![&p1, &p2, &p3, &p4];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 4);
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();

        // p4 should come first (no dependencies)
        assert_eq!(names[0], Some("p4"));
        // p2 and p3 should come after p4 (can be in any order)
        assert!(names[1..3].contains(&Some("p2")));
        assert!(names[1..3].contains(&Some("p3")));
        // p1 should come last
        assert_eq!(names[3], Some("p1"));
    }

    #[test]
    fn test_sort_partial_dependencies() {
        // Some projects have dependencies, some don't
        let p1 = create_project("p1", vec![]);
        let p2 = create_project("p2", vec!["p1"]);
        let p3 = create_project("p3", vec![]);
        let p4 = create_project("p4", vec!["p2"]);

        let projects = vec![&p4, &p3, &p2, &p1];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 4);
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();

        // p1 and p3 should come first (no dependencies, can be in any order)
        assert!(names[0..2].contains(&Some("p1")));
        assert!(names[0..2].contains(&Some("p3")));
        // p2 should come after p1
        let p2_idx = names.iter().position(|&n| n == Some("p2")).unwrap();
        let p1_idx = names.iter().position(|&n| n == Some("p1")).unwrap();
        assert!(p2_idx > p1_idx);
        // p4 should come last
        assert_eq!(names[3], Some("p4"));
    }

    #[test]
    fn test_sort_missing_dependency() {
        // p1 depends on "missing" which doesn't exist in the projects list
        let p1 = create_project("p1", vec!["missing"]);
        let p2 = create_project("p2", vec![]);

        let projects = vec![&p1, &p2];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 2);
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();

        // Both should be in the result (missing dependency is ignored, so both have in_degree 0)
        assert!(names.contains(&Some("p1")));
        assert!(names.contains(&Some("p2")));
        // Since both have no valid dependencies (p1's dependency doesn't exist), order may vary
    }

    #[test]
    fn test_sort_single_project() {
        let p1 = create_project("p1", vec![]);

        let projects = vec![&p1];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name(), Some("p1"));
    }

    #[test]
    fn test_sort_self_reference_ignored() {
        // p1 depends on itself (should be ignored as it's not in the name_to_index map correctly)
        let p1 = create_project("p1", vec!["p1"]);
        let p2 = create_project("p2", vec![]);

        let projects = vec![&p1, &p2];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 2);
        // Both should be in the result, order may vary but both should be present
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();
        assert!(names.contains(&Some("p1")));
        assert!(names.contains(&Some("p2")));
    }

    #[test]
    fn test_sort_cyclic_dependency() {
        // p1 -> p2 -> p3 -> p1 (circular dependency)
        let p1 = create_project("p1", vec!["p3"]);
        let p2 = create_project("p2", vec!["p1"]);
        let p3 = create_project("p3", vec!["p2"]);

        let projects = vec![&p1, &p2, &p3];
        let sorted = sort_by_dependencies(projects);

        // All projects should still be in the result even with cyclic deps
        assert_eq!(sorted.len(), 3);
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();
        assert!(names.contains(&Some("p1")));
        assert!(names.contains(&Some("p2")));
        assert!(names.contains(&Some("p3")));
    }

    #[test]
    fn test_sort_diamond_dependency_with_multiple_queue_entries() {
        // Diamond pattern where a project might be added to queue multiple times
        // p1 -> p2, p3
        // p2 -> p4
        // p3 -> p4
        // p4 -> p5
        // p5 -> (no deps)
        // When p4's in_degree becomes 0, it might be added from both p2 and p3 processing
        let p5 = create_project("p5", vec![]);
        let p4 = create_project("p4", vec!["p5"]);
        let p3 = create_project("p3", vec!["p4"]);
        let p2 = create_project("p2", vec!["p4"]);
        let p1 = create_project("p1", vec!["p2", "p3"]);

        let projects = vec![&p1, &p2, &p3, &p4, &p5];
        let sorted = sort_by_dependencies(projects);

        assert_eq!(sorted.len(), 5);
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();

        // p5 should come first
        assert_eq!(names[0], Some("p5"));
        // p4 should come after p5
        let p4_idx = names.iter().position(|&n| n == Some("p4")).unwrap();
        let p5_idx = names.iter().position(|&n| n == Some("p5")).unwrap();
        assert!(p4_idx > p5_idx);
        // p1 should come last
        assert_eq!(names[4], Some("p1"));
    }
}
