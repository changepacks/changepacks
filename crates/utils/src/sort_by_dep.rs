use changepacks_core::Project;
use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};

/// A project participating in a dependency cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyCycleMember {
    pub name: String,
    pub path: PathBuf,
}

/// Deterministic details for every project that participates in a dependency cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyCycleError {
    members: Vec<DependencyCycleMember>,
}

impl DependencyCycleError {
    #[must_use]
    pub fn members(&self) -> &[DependencyCycleMember] {
        &self.members
    }
}

impl fmt::Display for DependencyCycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "dependency cycle detected: ")?;
        for (index, member) in self.members.iter().enumerate() {
            if index > 0 {
                write!(formatter, ", ")?;
            }
            write!(formatter, "{} ({})", member.name, member.path.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for DependencyCycleError {}

/// Deterministic details for a dependency name that matches multiple projects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyAmbiguityError {
    dependency: String,
    candidates: Vec<PathBuf>,
}

impl DependencyAmbiguityError {
    #[must_use]
    pub fn dependency(&self) -> &str {
        &self.dependency
    }

    #[must_use]
    pub fn candidates(&self) -> &[PathBuf] {
        &self.candidates
    }
}

impl fmt::Display for DependencyAmbiguityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ambiguous dependency `{}`: candidates: ",
            self.dependency
        )?;
        for (index, candidate) in self.candidates.iter().enumerate() {
            if index > 0 {
                write!(formatter, ", ")?;
            }
            write!(formatter, "{}", candidate.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for DependencyAmbiguityError {}

/// An error that prevents projects from having a unique dependency ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySortError {
    Cycle(DependencyCycleError),
    AmbiguousDependency(DependencyAmbiguityError),
}

impl DependencySortError {
    /// Cycle members, or an empty slice when this is an ambiguity error.
    #[must_use]
    pub fn members(&self) -> &[DependencyCycleMember] {
        match self {
            Self::Cycle(error) => error.members(),
            Self::AmbiguousDependency(_) => &[],
        }
    }

    #[must_use]
    pub fn ambiguity(&self) -> Option<&DependencyAmbiguityError> {
        match self {
            Self::Cycle(_) => None,
            Self::AmbiguousDependency(error) => Some(error),
        }
    }
}

impl fmt::Display for DependencySortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle(error) => error.fmt(formatter),
            Self::AmbiguousDependency(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DependencySortError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cycle(error) => Some(error),
            Self::AmbiguousDependency(error) => Some(error),
        }
    }
}

/// Find cycle members with one iterative Kosaraju pass over the residual graph.
///
/// A node is cyclic when it belongs to an SCC with multiple nodes, or when its
/// singleton SCC has a self-edge. Residual nodes outside those SCCs are merely
/// blocked dependents and are intentionally left unmarked.
fn cycle_members_in_residual(adj: &[usize], offsets: &[usize], residual: &[bool]) -> Vec<bool> {
    let node_count = residual.len();
    let mut visited = vec![false; node_count];
    let mut finish_order = Vec::with_capacity(node_count);
    let mut dfs_stack: Vec<(usize, usize)> = Vec::new();

    for root in 0..node_count {
        if !residual[root] || visited[root] {
            continue;
        }

        visited[root] = true;
        dfs_stack.push((root, offsets[root]));
        while let Some((node, next_edge)) = dfs_stack.last_mut() {
            if *next_edge < offsets[*node + 1] {
                let target = adj[*next_edge];
                *next_edge += 1;
                if residual[target] && !visited[target] {
                    visited[target] = true;
                    dfs_stack.push((target, offsets[target]));
                }
            } else {
                let (finished, _) = dfs_stack.pop().expect("DFS stack is non-empty");
                finish_order.push(finished);
            }
        }
    }

    // Build the transpose CSR for residual edges only.
    let mut reverse_offsets = vec![0; node_count + 1];
    for source in 0..node_count {
        if !residual[source] {
            continue;
        }
        for &target in &adj[offsets[source]..offsets[source + 1]] {
            if residual[target] {
                reverse_offsets[target + 1] += 1;
            }
        }
    }
    for index in 1..reverse_offsets.len() {
        reverse_offsets[index] += reverse_offsets[index - 1];
    }

    let mut reverse_cursor = reverse_offsets.clone();
    let mut reverse_adj = vec![0; reverse_offsets[node_count]];
    for source in 0..node_count {
        if !residual[source] {
            continue;
        }
        for &target in &adj[offsets[source]..offsets[source + 1]] {
            if residual[target] {
                let slot = reverse_cursor[target];
                reverse_adj[slot] = source;
                reverse_cursor[target] += 1;
            }
        }
    }

    let mut assigned = vec![false; node_count];
    let mut cycle_members = vec![false; node_count];
    let mut component = Vec::new();
    let mut stack = Vec::new();

    for root in finish_order.into_iter().rev() {
        if assigned[root] {
            continue;
        }

        assigned[root] = true;
        stack.push(root);
        while let Some(node) = stack.pop() {
            component.push(node);
            for &source in &reverse_adj[reverse_offsets[node]..reverse_offsets[node + 1]] {
                if !assigned[source] {
                    assigned[source] = true;
                    stack.push(source);
                }
            }
        }

        let cyclic = component.len() > 1 || adj[offsets[root]..offsets[root + 1]].contains(&root);
        if cyclic {
            for &node in &component {
                cycle_members[node] = true;
            }
        }
        component.clear();
    }

    cycle_members
}

/// Sort projects by their dependencies using topological sort.
/// Projects with no dependencies or whose dependencies are already published will come first.
/// Returns project references in dependency order, or deterministic cycle/ambiguity details.
pub fn sort_by_dependencies(projects: Vec<&Project>) -> Result<Vec<&Project>, DependencySortError> {
    if projects.is_empty() {
        return Ok(projects);
    }

    // Dependencies are stored as package names, so name lookup is the ordering key.
    // name_to_index maps each name to Some(idx) if unique, or None if duplicate/ambiguous.
    let mut name_to_index: HashMap<&str, Option<usize>> = HashMap::with_capacity(projects.len());
    for (idx, project) in projects.iter().enumerate() {
        if let Some(name) = project.name() {
            match name_to_index.entry(name) {
                Entry::Occupied(e) => {
                    *e.into_mut() = None;
                }
                Entry::Vacant(v) => {
                    v.insert(Some(idx));
                }
            }
        }
    }

    // Build dependency graph: for each project, find which projects depend on it.
    // Duplicate names are ambiguous across polyglot publish sets and are marked None
    // in name_to_index. Only names actually referenced by an edge are errors.
    // in_degree[i] = number of dependencies that project i has
    let mut in_degree: Vec<usize> = vec![0; projects.len()];
    // Collect edges in the same order the old adjacency Vecs received pushes:
    // project-major, then dependency iteration order.
    let dependency_count: usize = projects
        .iter()
        .map(|project| project.dependencies().len())
        .sum();
    let mut edges: Vec<(usize, usize)> = Vec::with_capacity(dependency_count);
    let mut ambiguous_dependencies: Vec<&str> = Vec::new();

    for (idx, project) in projects.iter().enumerate() {
        let deps = project.dependencies();
        for dep in deps {
            match name_to_index.get(dep.as_str()) {
                Some(Some(dep_idx)) => {
                    // Project at idx depends on project at dep_idx
                    // So dep_idx should come before idx
                    edges.push((*dep_idx, idx));
                    in_degree[idx] += 1;
                }
                Some(None) => ambiguous_dependencies.push(dep),
                None => {}
            }
        }
    }

    if !ambiguous_dependencies.is_empty() {
        ambiguous_dependencies.sort_unstable();
        let dependency = ambiguous_dependencies[0];
        let mut candidates: Vec<_> = projects
            .iter()
            .filter(|project| project.name() == Some(dependency))
            .map(|project| project.relative_path().to_path_buf())
            .collect();
        candidates.sort_by(|left, right| compare_paths(left, right));
        return Err(DependencySortError::AmbiguousDependency(
            DependencyAmbiguityError {
                dependency: dependency.to_string(),
                candidates,
            },
        ));
    }

    // Store adjacency as CSR: adj[offsets[i]..offsets[i + 1]] contains the
    // projects that depend on project i. Stable counting-sort fill preserves
    // the old graph[dep_idx].push(idx) order within each source exactly.
    let mut offsets: Vec<usize> = vec![0; projects.len() + 1];
    for &(dep_idx, _) in &edges {
        offsets[dep_idx + 1] += 1;
    }
    for idx in 1..offsets.len() {
        offsets[idx] += offsets[idx - 1];
    }

    let mut cursor = offsets.clone();
    let mut adj: Vec<usize> = vec![0; edges.len()];
    for (dep_idx, dependent_idx) in edges {
        let slot = cursor[dep_idx];
        adj[slot] = dependent_idx;
        cursor[dep_idx] += 1;
    }

    // Kahn's algorithm for topological sort
    let mut queue: VecDeque<usize> = VecDeque::with_capacity(projects.len());
    for (idx, &degree) in in_degree.iter().enumerate() {
        if degree == 0 {
            queue.push_back(idx);
        }
    }

    // Kahn's traversal pushes every index in `0..projects.len()` at most once,
    // so the final length is bounded by `projects.len()`. Preallocating up front removes
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
    while let Some(idx) = queue.pop_front() {
        sorted_indices.push(idx);

        // Decrease in-degree of dependent projects
        for &dependent_idx in &adj[offsets[idx]..offsets[idx + 1]] {
            in_degree[dependent_idx] -= 1;
            if in_degree[dependent_idx] == 0 {
                queue.push_back(dependent_idx);
            }
        }
    }

    if sorted_indices.len() < projects.len() {
        let residual: Vec<bool> = in_degree.iter().map(|&degree| degree > 0).collect();
        let cycle_members = cycle_members_in_residual(&adj, &offsets, &residual);
        let mut members: Vec<_> = in_degree
            .iter()
            .enumerate()
            .filter(|(index, _)| cycle_members[*index])
            .map(|(index, _)| DependencyCycleMember {
                name: projects[index].name().unwrap_or("<unnamed>").to_string(),
                path: projects[index].relative_path().to_path_buf(),
            })
            .collect();
        members.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| compare_paths(&left.path, &right.path))
        });
        return Err(DependencySortError::Cycle(DependencyCycleError { members }));
    }

    // Reorder projects based on sorted indices (no cloning, just reordering references).
    // `sorted_indices` is dropped immediately after this expression, so
    // consuming it via `into_iter()` yields `usize` (Copy) values directly
    // and drops the `|&idx|` pattern. Zero perf change (compiler already
    // elides), but the intent — "consume the vector" — reads clearer than
    // "borrow every element then drop the borrow".
    Ok(sorted_indices
        .into_iter()
        .map(|idx| projects[idx])
        .collect())
}

fn path_sort_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn compare_paths(left: &Path, right: &Path) -> Ordering {
    path_sort_key(left)
        .cmp(&path_sort_key(right))
        .then_with(|| left.to_string_lossy().cmp(&right.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::create_project;
    use changepacks_node::package::NodePackage;

    fn create_project_at(name: &str, relative_path: &str) -> Project {
        Project::Package(Box::new(NodePackage::new(
            Some(name.to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test").join(relative_path),
            PathBuf::from(relative_path),
        )))
    }

    #[test]
    fn test_sort_empty() {
        let projects: Vec<&Project> = vec![];
        let sorted = sort_by_dependencies(projects).unwrap();
        assert_eq!(sorted.len(), 0);
    }

    #[test]
    fn test_sort_no_dependencies() {
        let p1 = create_project("p1", vec![]);
        let p2 = create_project("p2", vec![]);
        let p3 = create_project("p3", vec![]);

        let projects = vec![&p3, &p1, &p2];
        let sorted = sort_by_dependencies(projects).unwrap();

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
        let sorted = sort_by_dependencies(projects).unwrap();

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
        let sorted = sort_by_dependencies(projects).unwrap();

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
        let sorted = sort_by_dependencies(projects).unwrap();

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
        let sorted = sort_by_dependencies(projects).unwrap();

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
        let sorted = sort_by_dependencies(projects).unwrap();

        assert_eq!(sorted.len(), 2);
        let names: Vec<Option<&str>> = sorted.iter().map(|p| p.name()).collect();

        // Both should be in the result (missing dependency is ignored, so both have in_degree 0)
        assert!(names.contains(&Some("p1")));
        assert!(names.contains(&Some("p2")));
        // Since both have no valid dependencies (p1's dependency doesn't exist), order may vary
    }

    #[test]
    fn test_sort_rejects_referenced_ambiguous_dependency() {
        let core_a = create_project_at("core", "packages/core/package.json");
        let core_b = create_project_at("core", "crates/core/Cargo.toml");
        let app = create_project("app", vec!["core"]);

        let projects = vec![&app, &core_a, &core_b];
        let error = sort_by_dependencies(projects).expect_err("ambiguous edge must fail");
        let ambiguity = error.ambiguity().expect("ambiguity details");

        assert_eq!(ambiguity.dependency(), "core");
        assert_eq!(
            ambiguity.candidates(),
            [
                PathBuf::from("crates/core/Cargo.toml"),
                PathBuf::from("packages/core/package.json"),
            ]
        );
        assert_eq!(
            error.to_string(),
            "ambiguous dependency `core`: candidates: crates/core/Cargo.toml, packages/core/package.json"
        );
    }

    #[test]
    fn test_ambiguous_dependency_diagnostic_is_deterministic() {
        let zeta = create_project_at("shared", "zeta/package.json");
        let alpha = create_project_at("shared", "alpha/package.json");
        let app = create_project("app", vec!["shared"]);

        let first = sort_by_dependencies(vec![&app, &zeta, &alpha])
            .expect_err("ambiguous edge must fail")
            .to_string();
        let second = sort_by_dependencies(vec![&alpha, &app, &zeta])
            .expect_err("ambiguous edge must fail")
            .to_string();

        assert_eq!(first, second);
        assert_eq!(
            first,
            "ambiguous dependency `shared`: candidates: alpha/package.json, zeta/package.json"
        );
    }

    #[test]
    fn test_ambiguous_dependency_diagnostic_breaks_normalized_path_ties() {
        let slash = create_project_at("shared", "a/b/package.json");
        let backslash = create_project_at("shared", "a\\b/package.json");
        let app = create_project("app", vec!["shared"]);

        let first = sort_by_dependencies(vec![&app, &slash, &backslash])
            .expect_err("ambiguous edge must fail");
        let second = sort_by_dependencies(vec![&app, &backslash, &slash])
            .expect_err("ambiguous edge must fail");

        let first_candidates: Vec<_> = first
            .ambiguity()
            .expect("ambiguity details")
            .candidates()
            .iter()
            .map(|path| path.to_string_lossy())
            .collect();
        let second_candidates: Vec<_> = second
            .ambiguity()
            .expect("ambiguity details")
            .candidates()
            .iter()
            .map(|path| path.to_string_lossy())
            .collect();

        assert_eq!(first_candidates, second_candidates);
        assert_eq!(first.to_string(), second.to_string());
    }

    #[test]
    fn test_sort_allows_unreferenced_duplicate_names() {
        let core_a = create_project_at("core", "packages/core/package.json");
        let core_b = create_project_at("core", "crates/core/Cargo.toml");
        let app = create_project("app", vec![]);

        let sorted = sort_by_dependencies(vec![&app, &core_a, &core_b]).unwrap();
        let paths: Vec<_> = sorted
            .iter()
            .map(|project| project.relative_path())
            .collect();

        assert_eq!(
            paths,
            vec![
                Path::new("app/package.json"),
                Path::new("packages/core/package.json"),
                Path::new("crates/core/Cargo.toml"),
            ]
        );
    }

    #[test]
    fn test_sort_single_project() {
        let p1 = create_project("p1", vec![]);

        let projects = vec![&p1];
        let sorted = sort_by_dependencies(projects).unwrap();

        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name(), Some("p1"));
    }

    #[test]
    fn test_sort_self_reference_ignored() {
        // p1 depends on itself: `p1` IS in `name_to_index`, so the self-edge
        // gives p1 an in_degree of 1 that Kahn's loop never drains (its only
        // dependency is itself). The trailing cyclic-fallback loop then appends
        // it, so both projects still appear in the result.
        let p1 = create_project("p1", vec!["p1"]);
        let p2 = create_project("p2", vec![]);

        let projects = vec![&p1, &p2];
        let error = sort_by_dependencies(projects).expect_err("self-cycle must fail");

        assert_eq!(error.members()[0].name, "p1");
    }

    #[test]
    fn test_sort_cyclic_dependency() {
        // p1 -> p2 -> p3 -> p1 (circular dependency)
        let p1 = create_project("p1", vec!["p3"]);
        let p2 = create_project("p2", vec!["p1"]);
        let p3 = create_project("p3", vec!["p2"]);

        let projects = vec![&p1, &p2, &p3];
        let error = sort_by_dependencies(projects).expect_err("cycle must fail");

        assert_eq!(error.members().len(), 3);
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
        let sorted = sort_by_dependencies(projects).unwrap();

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

    #[test]
    fn test_sort_dag_returns_dependency_order() {
        let leaf = create_project("leaf", vec![]);
        let middle = create_project("middle", vec!["leaf"]);
        let root = create_project("root", vec!["middle"]);

        let sorted = sort_by_dependencies(vec![&root, &leaf, &middle])
            .expect("a DAG must have a topological ordering");
        let names: Vec<_> = sorted.iter().map(|project| project.name()).collect();

        assert_eq!(names, vec![Some("leaf"), Some("middle"), Some("root")]);
    }

    #[test]
    fn test_sort_rejects_self_cycle() {
        let project = create_project("self", vec!["self"]);

        let error = sort_by_dependencies(vec![&project]).expect_err("self-cycle must fail");

        assert_eq!(error.members().len(), 1);
        assert_eq!(error.members()[0].name, "self");
        assert_eq!(
            error.members()[0].path,
            std::path::Path::new("self/package.json")
        );
    }

    #[test]
    fn test_sort_rejects_multi_node_cycle() {
        let alpha = create_project("alpha", vec!["beta"]);
        let beta = create_project("beta", vec!["gamma"]);
        let gamma = create_project("gamma", vec!["alpha"]);

        let error = sort_by_dependencies(vec![&alpha, &beta, &gamma])
            .expect_err("multi-node cycle must fail");

        assert_eq!(
            error
                .members()
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn test_cycle_details_are_deterministic_and_exclude_blocked_dependents() {
        let zeta = create_project("zeta", vec!["alpha"]);
        let alpha = create_project("alpha", vec!["zeta"]);
        let blocked = create_project("blocked", vec!["zeta"]);

        let error =
            sort_by_dependencies(vec![&zeta, &blocked, &alpha]).expect_err("cycle must fail");

        let details: Vec<_> = error
            .members()
            .iter()
            .map(|member| (member.name.as_str(), member.path.as_path()))
            .collect();
        assert_eq!(
            details,
            vec![
                ("alpha", std::path::Path::new("alpha/package.json")),
                ("zeta", std::path::Path::new("zeta/package.json")),
            ]
        );
        assert_eq!(
            error.to_string(),
            "dependency cycle detected: alpha (alpha/package.json), zeta (zeta/package.json)"
        );
    }

    #[test]
    fn test_residual_scc_membership_marks_only_cycles() {
        // 0 <-> 1 is cyclic, 2 is blocked by that cycle, 3 has a self-edge,
        // and 4 is an acyclic residual singleton.
        let offsets = vec![0, 1, 3, 4, 5, 5];
        let adj = vec![1, 0, 2, 4, 3];
        let residual = vec![true; 5];

        assert_eq!(
            cycle_members_in_residual(&adj, &offsets, &residual),
            vec![true, true, false, true, false]
        );
    }

    #[test]
    fn test_cycle_error_excludes_a_chain_blocked_by_the_cycle() {
        let cycle_a = create_project("cycle-a", vec!["cycle-b"]);
        let cycle_b = create_project("cycle-b", vec!["cycle-a"]);
        let blocked_a = create_project("blocked-a", vec!["cycle-a"]);
        let blocked_b = create_project("blocked-b", vec!["blocked-a"]);

        let error = sort_by_dependencies(vec![&blocked_b, &cycle_b, &blocked_a, &cycle_a])
            .expect_err("cycle must fail");

        assert_eq!(
            error
                .members()
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            vec!["cycle-a", "cycle-b"]
        );
    }

    #[test]
    fn test_singleton_scc_requires_a_self_edge() {
        let self_cycle = create_project("self-cycle", vec!["self-cycle"]);
        let blocked = create_project("blocked", vec!["self-cycle"]);

        let error =
            sort_by_dependencies(vec![&blocked, &self_cycle]).expect_err("self-cycle must fail");

        assert_eq!(
            error
                .members()
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            vec!["self-cycle"]
        );
    }

    #[test]
    fn test_disjoint_cycles_are_all_reported_in_deterministic_order() {
        let zeta = create_project_at("zeta", "z/zeta.json");
        let alpha = create_project_at("alpha", "a/alpha.json");
        let delta = create_project_at("delta", "d/delta.json");
        let beta = create_project_at("beta", "b/beta.json");

        let mut zeta = zeta;
        let mut alpha = alpha;
        let mut delta = delta;
        let mut beta = beta;
        zeta.add_dependency("alpha");
        alpha.add_dependency("zeta");
        delta.add_dependency("beta");
        beta.add_dependency("delta");

        let error = sort_by_dependencies(vec![&zeta, &delta, &alpha, &beta])
            .expect_err("disjoint cycles must fail");

        assert_eq!(
            error
                .members()
                .iter()
                .map(|member| (member.name.as_str(), member.path.as_path()))
                .collect::<Vec<_>>(),
            vec![
                ("alpha", Path::new("a/alpha.json")),
                ("beta", Path::new("b/beta.json")),
                ("delta", Path::new("d/delta.json")),
                ("zeta", Path::new("z/zeta.json")),
            ]
        );
    }

    #[test]
    fn test_large_cycle_reports_every_member() {
        const SIZE: usize = 4_096;
        let mut projects: Vec<_> = (0..SIZE)
            .map(|index| create_project(&format!("cycle-{index:04}"), vec![]))
            .collect();
        for (index, project) in projects.iter_mut().enumerate() {
            project.add_dependency(&format!("cycle-{:04}", (index + 1) % SIZE));
        }
        let refs = projects.iter().collect();

        let error = sort_by_dependencies(refs).expect_err("large cycle must fail");

        assert_eq!(error.members().len(), SIZE);
        assert_eq!(error.members().first().unwrap().name, "cycle-0000");
        assert_eq!(error.members().last().unwrap().name, "cycle-4095");
    }
}
