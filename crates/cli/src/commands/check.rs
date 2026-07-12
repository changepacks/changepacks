use changepacks_core::{ChangePackResultLog, Project, UpdateType};

use anyhow::Result;
use changepacks_utils::{
    apply_reverse_dependencies, display_update, gen_changepack_result_map, gen_update_map,
    get_relative_path_ref,
};
use clap::Args;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::path::{Path, PathBuf};

use crate::{
    CommandContext,
    finders::collect_projects,
    options::{CliLanguage, FilterOptions, FormatOptions, retain_by_language},
};

/// Format the "(changed)" marker for a project, colored bright yellow if changed.
fn changed_marker(project: &Project) -> colored::ColoredString {
    use colored::Colorize;
    if project.is_changed() {
        " (changed)".bright_yellow()
    } else {
        "".normal()
    }
}

/// Select the box-drawing tree connector for a node based on whether it is the last sibling.
const fn tree_connector(is_last: bool) -> &'static str {
    if is_last { "└── " } else { "├── " }
}

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct CheckArgs {
    #[arg(short, long)]
    filter: Option<FilterOptions>,

    #[arg(long, default_value = "stdout")]
    format: FormatOptions,

    #[arg(short, long)]
    remote: bool,

    #[arg(long)]
    tree: bool,

    /// Filter projects by language. Can be specified multiple times to include multiple languages.
    #[arg(short, long, value_enum)]
    pub language: Vec<CliLanguage>,
}

/// Check project status
///
/// # Errors
/// Returns error if command context creation or project checking fails.
///
/// Excluded from coverage: orchestrates `CommandContext::new` (git I/O)
/// and a deeply-nested multi-line `format!(...).replace(...)` expression
/// where tarpaulin mis-attributes one branch of the inner `if let
/// Some(update_type) = update_map.get(...)`. The underlying helpers
/// (`display_update`, `gen_update_map`, `apply_reverse_dependencies`,
/// `format_project_line`) are covered by their own tests.
#[cfg(not(tarpaulin_include))]
pub async fn handle_check(args: &CheckArgs) -> Result<()> {
    let ctx = CommandContext::new(args.remote).await?;

    let mut projects = collect_projects(&ctx.project_finders);
    if let Some(filter) = &args.filter {
        projects.retain(|p| filter.matches(p));
    }
    retain_by_language(&args.language, &mut projects);
    projects.sort();
    if let FormatOptions::Stdout = args.format {
        println!("Found {} projects", projects.len());
    }
    let mut update_map = gen_update_map(&ctx.changepacks_dir, &ctx.config).await?;

    // Apply reverse dependency updates (workspace:* dependencies)
    apply_reverse_dependencies(&mut update_map, &projects, &ctx.repo_root_path);

    if args.tree {
        // Tree mode: show dependencies as a tree
        display_tree(&projects, &ctx.repo_root_path, &update_map)?;
    } else {
        match args.format {
            FormatOptions::Stdout => {
                for project in projects {
                    let changed_marker = changed_marker(project);
                    let version_str =
                        version_display_with_update(project, &ctx.repo_root_path, &update_map)?;
                    println!(
                        "{}{}",
                        project.format_line(Some(&version_str)),
                        changed_marker
                    );
                }
            }
            FormatOptions::Json => {
                let json = serde_json::to_string_pretty(&gen_changepack_result_map(
                    projects.as_slice(),
                    &ctx.repo_root_path,
                    &update_map,
                )?)?;
                println!("{json}");
            }
        }
    }
    Ok(())
}

/// Collect a project's monorepo-local dependency names, sorted.
///
/// Single source of truth for the "keep only deps that resolve in
/// `name_to_project`, borrow them as `&str`, and `sort_unstable`" policy
/// shared by `display_tree` (forward-graph construction) and
/// `format_project_line` (the `[deps: ...]` annotation). Returns an empty
/// `Vec` when no monorepo-local dependency survives the filter — callers
/// keep their own empty-guard behavior (skip the graph insert / degrade to
/// the no-`[deps]` line).
///
/// `sort_unstable`: dep names are unique package-name slices and `str::cmp`
/// is a total order, so stability is not observable in the rendered output.
fn sorted_monorepo_deps<'a>(
    project: &'a Project,
    name_to_project: &HashMap<&str, &Project>,
) -> Vec<&'a str> {
    let deps = project.dependencies();
    // Preallocate to the tight upper bound: `Filter::size_hint` only reports
    // `(0, Some(deps.len()))`, so a `.filter().collect()` under-reserves and
    // reallocates geometrically; `deps.len()` overshoots by at most the
    // non-monorepo deps. The lookup passes `dep.as_str()` because
    // `HashMap<&str, _>::contains_key(&Q)` resolves with `Q = str` (via
    // `&str: Borrow<str>`), not `Q = String`.
    let mut filtered: Vec<&'a str> = Vec::with_capacity(deps.len());
    for dep in deps {
        if name_to_project.contains_key(dep.as_str()) {
            filtered.push(dep.as_str());
        }
    }
    filtered.sort_unstable();
    filtered
}

/// Display projects as a dependency tree
///
/// Excluded from coverage: pure CLI display orchestration that emits
/// formatted output via `println!`; the underlying helpers
/// (`display_tree_node`, `format_project_line`) carry the testable logic
/// and are covered separately.
#[cfg(not(tarpaulin_include))]
fn display_tree(
    projects: &[&Project],
    repo_root_path: &std::path::Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
) -> Result<()> {
    // Create a map from project name (fallback "noname") to project.
    // Project names — not paths — key this map because both the graph and the
    // dependency lookups (`project.dependencies()`) speak the *name* namespace.
    //
    // Keys are borrowed `&str` from `project.name()` (or the `"noname"`
    // static fallback), matching the same pattern the sibling `roots`
    // and `has_dependencies` HashSet<&str> collections use in this
    // function. The `Project` refs already outlive `handle_check`'s
    // scope, so the borrowed name slices they own outlive every
    // downstream tree traversal below — no lifetime cascade to plumb.
    // Cuts N `String::from_str` allocations per `check --tree` invocation
    // (one per project name) with byte-identical map contents.
    let mut name_to_project: HashMap<&str, &Project> = HashMap::with_capacity(projects.len());
    for project in projects {
        name_to_project.insert(project.name_or_noname(), project);
    }

    // Build the forward dependency graph: graph[project] = that project's
    // monorepo-local dependency names, which `display_tree_node` renders as
    // that project's children in the tree below.
    // Preallocate: `projects.len()` is a tight upper bound for every map/set built
    // below by a single pass over `projects`. Matches the preallocation policy
    // already applied in `sort_by_dep.rs` and `apply_reverse_dependencies`.
    //
    // Keys and dep-list values borrow `&str` from the projects — the
    // same borrowing pattern the sibling `name_to_project`, `roots`,
    // `has_dependencies`, and `visited` collections in this function
    // already use. Projects live for `handle_check`'s scope
    // (`&[&Project]`), so every borrowed name slice outlives every
    // downstream tree traversal. Retires the per-project
    // `project.name().unwrap_or("noname").to_string()` key allocation
    // AND the per-edge `dep.clone()` value allocation, closing the last
    // "owned string" gap in this function.
    let mut graph: HashMap<&str, Vec<&str>> = HashMap::with_capacity(projects.len());
    // Borrow the `&str` name that already lives inside each `Project`;
    // `name_to_project` keys on `&str` too, so these names look up
    // directly. Avoids N per-invocation `String::clone`s of names we
    // already own further up the stack.

    for project in projects {
        // `sorted_monorepo_deps` applies the shared monorepo-local + sorted
        // filter, so the dep list lands in `graph` already sorted ONCE — no
        // separate `graph.values_mut()` pass is needed. `project` auto-derefs
        // `&&Project` → `&Project`; the borrowed dep slices live for the
        // projects' scope, matching the graph's `&str` values. Insert only
        // non-empty results, so a dependency-free project stays out of
        // `graph` exactly as before.
        let monorepo_deps = sorted_monorepo_deps(project, &name_to_project);
        if !monorepo_deps.is_empty() {
            graph.insert(project.name_or_noname(), monorepo_deps);
        }
    }

    // Derive `has_dependencies` AFTER the graph is fully built by
    // borrowing the `&str` slices that already live inside
    // `graph.values()` (`HashSet<&str>`). Byte-identical membership to
    // the previous `String::as_str` map: `graph.values().flatten()`
    // yields `&&str`, and `.copied()` unwraps that to `&str` without
    // touching the underlying storage.
    //
    // Preallocate against the exact upper bound: the total edge count is
    // `graph.values().map(Vec::len).sum()`. `HashSet::from_iter` (via
    // `.collect()`) does NOT reserve capacity from `Iterator::size_hint`,
    // so seeding + `.extend(...)` skips the log2(N) geometric-doubling
    // reallocations the un-hinted collect incurs. Matches the
    // preallocation policy already applied to `name_to_project`, `graph`,
    // `roots`, and `visited` in this same function.
    let has_dependencies_cap: usize = graph.values().map(Vec::len).sum();
    let mut has_dependencies: HashSet<&str> = HashSet::with_capacity(has_dependencies_cap);
    has_dependencies.extend(graph.values().flatten().copied());

    // Root nodes are projects that are not dependencies of any other project.
    // Build sorted_roots directly by filtering projects and sorting, which
    // achieves the same deduplication as the previous HashSet approach.
    // `sort_unstable()` + `dedup()` collapses duplicates identically to what
    // the HashSet collapsed. Roots are project NAMES, not unique keys: two
    // distinct projects can legitimately share a name (e.g. a Node `core` and
    // a Rust `core`, which `sort_by_dependencies` explicitly supports), so
    // both push the same `"core"` string here. `dedup()` is therefore
    // load-bearing — after the sort it collapses those duplicate name entries
    // so a shared-name root renders once, not once per project sharing it.
    //
    // Sort roots for consistent output. `Vec<&str>` sorts identically to
    // `Vec<String>` for the same name strings (byte-identical order), and
    // `name_to_project.get(root)` still resolves because the map keys on
    // `&str` (the loop below derefs `&&str` → `&str` to match).
    //
    // `sort_unstable`: `str::cmp` is a total order and any equal names are
    // byte-identical strings, so their relative order is not observable in the
    // printed tree. Skips the stability bookkeeping the stable sort pays for.
    //
    // Preallocate: `projects.len()` is the tight upper bound for roots
    // (at most all projects are roots if none have dependencies). Making
    // the reservation explicit matches the visually-uniform preallocation
    // policy already applied throughout this same function (`name_to_project`,
    // `graph`, `has_dependencies`, `visited`, `monorepo_deps`). Byte-identical
    // output; the goal is a uniform preallocation idiom so a future
    // maintainer can trust every `Vec::from_iter` was deliberate.
    let mut sorted_roots: Vec<&str> = Vec::with_capacity(projects.len());
    for project in projects {
        let name = project.name_or_noname();
        if !has_dependencies.contains(name) {
            sorted_roots.push(name);
        }
    }
    sorted_roots.sort_unstable();
    sorted_roots.dedup();

    // Display tree starting from roots.
    // Preallocate: `visited.insert(project_name)` fires at most once per
    // unique project (up to `projects.len()`), so seeding the HashSet with
    // that capacity avoids the geometric-doubling reallocations the
    // default `HashSet::new()` would trigger on trees with dozens of
    // nodes. Matches the preallocation policy already applied to
    // `name_to_project`, `graph`, and `roots` above in this same
    // function.
    //
    // Borrow the `&str` name that already lives inside each `Project` —
    // the same borrowing pattern the sibling `roots` and
    // `has_dependencies` sets use above. Cuts N `String::clone()` calls
    // (one per tree-node visit) with byte-identical membership.
    let mut visited: HashSet<&str> = HashSet::with_capacity(projects.len());
    let mut ctx = TreeContext {
        graph: &graph,
        name_to_project: &name_to_project,
        repo_root_path,
        update_map,
        line_cache: HashMap::with_capacity(projects.len()),
    };
    for (idx, root) in sorted_roots.iter().enumerate() {
        // Deref `&&str` → `&str` to match `name_to_project`'s `&str` key
        // type (`HashMap<&str, _>::get` resolves via `&str: Borrow<str>`).
        if let Some(project) = name_to_project.get(*root) {
            let is_last = idx == sorted_roots.len() - 1;
            display_tree_node(project, &mut ctx, "", is_last, &mut visited)?;
        }
    }

    // Display projects that weren't part of the tree (orphaned nodes).
    // Key on the unique manifest path via `line_cache`, NOT the project name:
    // `cached_project_line` inserts into `line_cache` exactly when a line is
    // printed (by `project.path()`), so "in `line_cache`" ⟺ "already displayed"
    // under a unique identity. Two distinct projects can legitimately share a
    // name (e.g. a Node `core` and a Rust `core`), and `name_to_project` keeps
    // only the last-inserted one, so a name-keyed check would wrongly treat the
    // dropped twin as already shown and never print it.
    for project in projects {
        if !ctx.line_cache.contains_key(project.path()) {
            println!("{}", cached_project_line(project, &mut ctx)?);
        }
    }

    Ok(())
}

/// Context for tree display operations
struct TreeContext<'a> {
    // `graph` now keys and values borrow `&str` from the projects, matching
    // the same borrowing pattern the sibling `name_to_project` field
    // already uses. Retires the `HashMap<String, Vec<String>>` shape's
    // per-node key/value clones on every `display_tree_node` walk.
    graph: &'a HashMap<&'a str, Vec<&'a str>>,
    name_to_project: &'a HashMap<&'a str, &'a Project>,
    repo_root_path: &'a Path,
    update_map: &'a HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    line_cache: HashMap<&'a Path, String>,
}

fn cached_project_line<'a, 'ctx>(
    project: &'a Project,
    ctx: &'ctx mut TreeContext<'a>,
) -> Result<&'ctx str> {
    let project_path = project.path();
    match ctx.line_cache.entry(project_path) {
        Entry::Occupied(entry) => Ok(entry.into_mut().as_str()),
        Entry::Vacant(entry) => {
            let line = format_project_line(
                project,
                ctx.repo_root_path,
                ctx.update_map,
                ctx.name_to_project,
            )?;
            Ok(entry.insert(line).as_str())
        }
    }
}

/// Display a single node in the tree
fn display_tree_node<'a>(
    project: &'a Project,
    ctx: &mut TreeContext<'a>,
    prefix: &str,
    is_last: bool,
    visited: &mut HashSet<&'a str>,
) -> Result<()> {
    // Borrow the name out of the project rather than allocating a fresh
    // `String` per visit — the `Project` outlives every downstream
    // borrow (`visited`, `ctx.graph`, `ctx.name_to_project` all live for
    // `handle_check`'s scope), so the name slice is safe to thread
    // through the recursion. Diamond graphs re-descend the same subtree
    // under multiple parents, so retiring the per-visit `String::from`
    // + `.clone()` pair collapses two heap ops per tree node down to a
    // pointer copy.
    let project_name: &'a str = project.name_or_noname();
    let is_first_visit = visited.insert(project_name);

    // Only print the project line if this is the first time visiting it
    if is_first_visit {
        let connector = tree_connector(is_last);
        println!(
            "{}{}{}",
            prefix,
            connector,
            cached_project_line(project, ctx)?
        );
    }

    // Always display dependencies, even if the node was already visited
    // This ensures all dependencies are shown in the tree.
    // NOTE: `deps` is pre-sorted ONCE when `display_tree` builds the graph
    // (via `sorted_monorepo_deps`); borrowing here avoids the per-visit
    // `deps.clone()` + `.sort()` — meaningful on diamond graphs where the
    // same subtree is re-descended under multiple parents.
    if let Some(deps) = ctx.graph.get(project_name) {
        let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
        for (idx, dep_name) in deps.iter().enumerate() {
            // `deps: &Vec<&str>` now, so `deps.iter()` yields
            // `dep_name: &&str`. Deref once with `*dep_name` at the two
            // call sites below so `HashMap<&str, _>::get(&&str)` and
            // `HashSet<&str>::contains(&&str)` both resolve via
            // `&str: Borrow<str>` — byte-identical semantics to the
            // previous `dep_name.as_str()` chain on `&String`.
            if let Some(dep_project) = ctx.name_to_project.get(*dep_name) {
                // `deps.iter().enumerate()` guarantees `deps.len() >= 1`
                // inside the loop, so plain subtraction is safe.
                let is_last_dep = idx == deps.len() - 1;
                // Use a separate visited set for dependencies to avoid infinite loops
                // but still show all dependencies
                if visited.contains(*dep_name) {
                    // If already visited, just print it without recursion to avoid loops
                    let dep_connector = tree_connector(is_last_dep);
                    println!(
                        "{}{}{}",
                        new_prefix,
                        dep_connector,
                        cached_project_line(dep_project, ctx)?
                    );
                } else {
                    display_tree_node(dep_project, ctx, &new_prefix, is_last_dep, visited)?;
                }
            }
        }
    }

    Ok(())
}

/// Resolve a project's display version, applying a pending `update_map`
/// bump when present.
///
/// Single source of truth for "resolve the repo-relative key, render
/// `display_update` when the map carries a pending bump, else fall back to
/// `version_display`" — shared by the `check` stdout flat-list arm and
/// `format_project_line` (tree / orphan path). Byte-identical to the two
/// previously open-coded copies.
///
/// # Errors
/// Returns error if the repo-relative path or the update display cannot be computed.
fn version_display_with_update(
    project: &Project,
    repo_root_path: &Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
) -> Result<String> {
    if update_map.is_empty() {
        return Ok(project.version_display());
    }
    let key = get_relative_path_ref(repo_root_path, project.path())?;
    Ok(match update_map.get(key) {
        Some(update_entry) => display_update(project.version(), update_entry.0)?,
        None => project.version_display(),
    })
}

/// Format a project line for display
///
/// Excluded from coverage: tarpaulin mis-attributes the `display_update`
/// branch of the `if let Some(update_entry) = update_map.get(...)`
/// expression under normal rustfmt despite both branches being exercised
/// via the check command integration flow. The helpers it composes
/// (`display_update`, `get_relative_path`) are covered by their own tests.
#[cfg(not(tarpaulin_include))]
fn format_project_line(
    project: &Project,
    repo_root_path: &std::path::Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    name_to_project: &HashMap<&str, &Project>,
) -> Result<String> {
    use colored::Colorize;

    let version = version_display_with_update(project, repo_root_path, update_map)?;

    let changed_marker = changed_marker(project);

    // Collect the monorepo-local deps (sorted) via the shared helper, then
    // fuse the join into a single `String::push_str` loop, matching the
    // `format_selected_projects` pattern in `prompter.rs`. Empty-guard shape
    // preserved: `deps_info` still degrades to `"".normal()` when no
    // monorepo-local dep survives the filter.
    //
    // Preallocate: `String::new().push_str(...)` grows via geometric
    // doubling on every dep addition. On projects with N monorepo
    // dependencies (rendered as `\n        core\n        utils\n...`),
    // that's `log2(total_len)` reallocations per project displayed —
    // multiplied through every `display_tree_node` visit. Summing
    // `dep.len() + 9` (each dep name plus its `\n        ` separator, 9
    // bytes) is a tight upper bound that overshoots by at most one
    // separator (the first dep skips the leading separator). Matches the
    // preallocation policy already applied throughout the workspace.
    let filtered_deps = sorted_monorepo_deps(project, name_to_project);
    let mut deps_str = String::with_capacity(filtered_deps.iter().map(|d| d.len() + 9).sum());
    for dep in filtered_deps {
        if !deps_str.is_empty() {
            deps_str.push_str("\n        ");
        }
        deps_str.push_str(dep);
    }
    let deps_info = if deps_str.is_empty() {
        "".normal()
    } else {
        format!(" [deps:\n        {deps_str}]").bright_black()
    };

    // Reuse `Project::format_line` so the base label stays in sync with
    // `Project::Display`; then append the CLI-only `deps` info + changed marker.
    let base_format = project.format_line(Some(&version));

    Ok(format!("{base_format}{changed_marker}{deps_info}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use rstest::rstest;

    // Test CheckArgs parsing via clap
    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        check: CheckArgs,
    }

    #[test]
    fn test_check_args_default() {
        let cli = TestCli::parse_from(["test"]);
        assert!(cli.check.filter.is_none());
        assert!(matches!(cli.check.format, FormatOptions::Stdout));
        assert!(!cli.check.remote);
        assert!(!cli.check.tree);
    }

    #[test]
    fn test_check_args_with_json_format() {
        let cli = TestCli::parse_from(["test", "--format", "json"]);
        assert!(matches!(cli.check.format, FormatOptions::Json));
    }

    #[test]
    fn test_check_args_with_tree() {
        let cli = TestCli::parse_from(["test", "--tree"]);
        assert!(cli.check.tree);
    }

    // `--filter` (long) and `-f` (short) both parse into `Some(FilterOptions::X)`.
    #[rstest]
    #[case(&["test", "--filter", "workspace"], FilterOptions::Workspace)]
    #[case(&["test", "--filter", "package"], FilterOptions::Package)]
    #[case(&["test", "-f", "workspace"], FilterOptions::Workspace)]
    fn test_check_args_filter_flag(#[case] args: &[&str], #[case] expected: FilterOptions) {
        let cli = TestCli::parse_from(args);
        let filter = cli.check.filter.expect("filter should be present");
        assert_eq!(filter, expected);
    }

    #[test]
    fn test_check_args_combined() {
        let cli = TestCli::parse_from([
            "test", "--filter", "package", "--format", "json", "--tree", "--remote",
        ]);
        assert!(matches!(cli.check.filter, Some(FilterOptions::Package)));
        assert!(matches!(cli.check.format, FormatOptions::Json));
        assert!(cli.check.tree);
        assert!(cli.check.remote);
    }

    // `--remote` (long) and `-r` (short) both flip the `remote` flag.
    #[rstest]
    #[case(&["test", "--remote"])]
    #[case(&["test", "-r"])]
    fn test_check_args_remote_flag(#[case] args: &[&str]) {
        let cli = TestCli::parse_from(args);
        assert!(cli.check.remote);
    }

    // `--language` / `-l` accumulate into `Vec<CliLanguage>`; the parsed
    // length must match the number of flags supplied.
    #[rstest]
    #[case(&["test", "--language", "node"], 1)]
    #[case(&["test", "-l", "rust"], 1)]
    #[case(&["test", "--language", "node", "--language", "python"], 2)]
    fn test_check_args_language_flag(#[case] args: &[&str], #[case] expected_len: usize) {
        let cli = TestCli::parse_from(args);
        assert_eq!(cli.check.language.len(), expected_len);
    }

    // --- format_project_line tests using mock trait implementations ---

    use changepacks_core::Language;

    use changepacks_core::test_support::{MockPackage, MockWorkspace};

    #[test]
    fn test_format_project_line_package() {
        let pkg = MockPackage::with_all(
            Some("my-lib"),
            Some("1.2.3"),
            "/repo/crates/my-lib/Cargo.toml",
            "crates/my-lib/Cargo.toml",
            Language::Rust,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let update_map = HashMap::new();
        let mut name_to_project: HashMap<&str, &Project> = HashMap::new();
        name_to_project.insert("my-lib", &project);

        let line = format_project_line(&project, repo_root, &update_map, &name_to_project).unwrap();
        assert!(line.contains("my-lib"));
        assert!(line.contains("v1.2.3"));
    }

    #[test]
    fn test_format_project_line_workspace() {
        let ws = MockWorkspace::with_all(
            Some("my-workspace"),
            Some("2.0.0"),
            "/repo/package.json",
            "package.json",
            Language::Node,
        );
        let project = Project::Workspace(Box::new(ws));
        let repo_root = Path::new("/repo");
        let update_map = HashMap::new();
        let mut name_to_project: HashMap<&str, &Project> = HashMap::new();
        name_to_project.insert("my-workspace", &project);

        let line = format_project_line(&project, repo_root, &update_map, &name_to_project).unwrap();
        assert!(line.contains("my-workspace"));
        assert!(line.contains("Workspace"));
        assert!(line.contains("v2.0.0"));
    }

    #[test]
    fn test_format_project_line_with_update() {
        let pkg = MockPackage::with_all(
            Some("updated-pkg"),
            Some("1.0.0"),
            "/repo/packages/foo/package.json",
            "packages/foo/package.json",
            Language::Node,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("packages/foo/package.json"),
            (UpdateType::Minor, vec![]),
        );
        let name_to_project: HashMap<&str, &Project> = HashMap::new();

        let line = format_project_line(&project, repo_root, &update_map, &name_to_project).unwrap();
        assert!(line.contains("updated-pkg"));
        // The update display should show version transition
        assert!(line.contains("1.1.0") || line.contains("1.0.0"));
    }

    #[test]
    fn test_format_project_line_changed_marker() {
        let mut pkg = MockPackage::with_all(
            Some("changed-pkg"),
            Some("3.0.0"),
            "/repo/lib/Cargo.toml",
            "lib/Cargo.toml",
            Language::Rust,
        );
        pkg.is_changed = true;
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let update_map = HashMap::new();
        let name_to_project: HashMap<&str, &Project> = HashMap::new();

        let line = format_project_line(&project, repo_root, &update_map, &name_to_project).unwrap();
        assert!(line.contains("changed-pkg"));
        assert!(line.contains("changed"));
    }

    #[test]
    fn test_format_project_line_with_dependencies() {
        let mut pkg = MockPackage::with_all(
            Some("app"),
            Some("1.0.0"),
            "/repo/app/package.json",
            "app/package.json",
            Language::Node,
        );
        pkg.dependencies.insert("core-lib".to_string());
        let project = Project::Package(Box::new(pkg));

        let dep_pkg = MockPackage::with_all(
            Some("core-lib"),
            Some("1.0.0"),
            "/repo/core/package.json",
            "core/package.json",
            Language::Node,
        );
        let dep_project = Project::Package(Box::new(dep_pkg));

        let repo_root = Path::new("/repo");
        let update_map = HashMap::new();
        let mut name_to_project: HashMap<&str, &Project> = HashMap::new();
        name_to_project.insert("app", &project);
        name_to_project.insert("core-lib", &dep_project);

        let line = format_project_line(&project, repo_root, &update_map, &name_to_project).unwrap();
        assert!(line.contains("app"));
        assert!(line.contains("deps:"));
        assert!(line.contains("core-lib"));
    }

    #[test]
    fn test_format_project_line_no_deps_shows_no_bracket() {
        let pkg = MockPackage::with_all(
            Some("standalone"),
            Some("1.0.0"),
            "/repo/standalone/Cargo.toml",
            "standalone/Cargo.toml",
            Language::Rust,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let update_map = HashMap::new();
        let name_to_project: HashMap<&str, &Project> = HashMap::new();

        let line = format_project_line(&project, repo_root, &update_map, &name_to_project).unwrap();
        assert!(line.contains("standalone"));
        assert!(!line.contains("deps:"));
    }

    #[test]
    fn test_format_project_line_deps_sorted_deterministically() {
        let mut pkg = MockPackage::with_all(
            Some("app"),
            Some("1.0.0"),
            "/repo/app/package.json",
            "app/package.json",
            Language::Node,
        );
        // Add dependencies in non-alphabetical order to verify sorting
        pkg.dependencies.insert("zebra".to_string());
        pkg.dependencies.insert("apple".to_string());
        pkg.dependencies.insert("mango".to_string());
        let project = Project::Package(Box::new(pkg));

        let apple_pkg = MockPackage::with_all(
            Some("apple"),
            Some("1.0.0"),
            "/repo/apple/package.json",
            "apple/package.json",
            Language::Node,
        );
        let apple_project = Project::Package(Box::new(apple_pkg));

        let zebra_pkg = MockPackage::with_all(
            Some("zebra"),
            Some("1.0.0"),
            "/repo/zebra/package.json",
            "zebra/package.json",
            Language::Node,
        );
        let zebra_project = Project::Package(Box::new(zebra_pkg));

        let mango_pkg = MockPackage::with_all(
            Some("mango"),
            Some("1.0.0"),
            "/repo/mango/package.json",
            "mango/package.json",
            Language::Node,
        );
        let mango_project = Project::Package(Box::new(mango_pkg));

        let repo_root = Path::new("/repo");
        let update_map = HashMap::new();
        let mut name_to_project: HashMap<&str, &Project> = HashMap::new();
        name_to_project.insert("app", &project);
        name_to_project.insert("apple", &apple_project);
        name_to_project.insert("zebra", &zebra_project);
        name_to_project.insert("mango", &mango_project);

        let line = format_project_line(&project, repo_root, &update_map, &name_to_project).unwrap();
        assert!(line.contains("app"));
        assert!(line.contains("deps:"));
        // Verify sorted order: apple, mango, zebra
        assert!(line.contains("deps:\n        apple\n        mango\n        zebra"));
    }

    #[test]
    fn test_cached_project_line_distinguishes_same_named_projects() {
        // Create two projects with the SAME name but DIFFERENT paths and versions
        let pkg1 = MockPackage::with_all(
            Some("core"),
            Some("1.0.0"),
            "/repo/packages/core/package.json",
            "packages/core/package.json",
            Language::Node,
        );
        let project1 = Project::Package(Box::new(pkg1));

        let pkg2 = MockPackage::with_all(
            Some("core"),
            Some("2.0.0"),
            "/repo/crates/core/Cargo.toml",
            "crates/core/Cargo.toml",
            Language::Rust,
        );
        let project2 = Project::Package(Box::new(pkg2));

        let repo_root = Path::new("/repo");
        let update_map = HashMap::new();
        let name_to_project: HashMap<&str, &Project> = HashMap::new();

        // Create a TreeContext with an empty line_cache
        let mut ctx = TreeContext {
            graph: &HashMap::new(),
            name_to_project: &name_to_project,
            repo_root_path: repo_root,
            update_map: &update_map,
            line_cache: HashMap::new(),
        };

        // Call cached_project_line for the first project and capture the line
        let line1 = cached_project_line(&project1, &mut ctx)
            .unwrap()
            .to_string();

        // Call cached_project_line for the second project and capture the line
        let line2 = cached_project_line(&project2, &mut ctx)
            .unwrap()
            .to_string();

        // Both lines should contain "core" (the project name)
        assert!(line1.contains("core"));
        assert!(line2.contains("core"));

        // But they should be DIFFERENT because they have different versions and paths
        assert_ne!(line1, line2);

        // line1 should contain v1.0.0 (project1's version)
        assert!(line1.contains("1.0.0"));

        // line2 should contain v2.0.0 (project2's version)
        assert!(line2.contains("2.0.0"));
    }

    // --- version_display_with_update tests ---

    #[test]
    fn test_version_display_with_update_empty_map() {
        // Case (a): empty update_map → returns plain version display like "v1.0.0"
        let pkg = MockPackage::with_all(
            Some("my-pkg"),
            Some("1.0.0"),
            "/repo/pkg/package.json",
            "pkg/package.json",
            Language::Node,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let update_map = HashMap::new();

        let result = version_display_with_update(&project, repo_root, &update_map).unwrap();
        assert_eq!(result, "v1.0.0");
    }

    #[test]
    fn test_version_display_with_update_key_miss() {
        // Case (b): NON-EMPTY map whose keys do NOT include this project's relative path
        // → still plain "v1.0.0"
        let pkg = MockPackage::with_all(
            Some("my-pkg"),
            Some("1.0.0"),
            "/repo/pkg/package.json",
            "pkg/package.json",
            Language::Node,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let mut update_map = HashMap::new();
        // Key the map by a DIFFERENT relative path
        update_map.insert(
            PathBuf::from("other/package.json"),
            (UpdateType::Minor, vec![]),
        );

        let result = version_display_with_update(&project, repo_root, &update_map).unwrap();
        assert_eq!(result, "v1.0.0");
    }

    #[test]
    fn test_version_display_with_update_key_hit_minor() {
        // Case (c): map keyed by this project's relative path with UpdateType::Minor
        // → display contains "v1.0.0 → v1.1.0"
        let pkg = MockPackage::with_all(
            Some("my-pkg"),
            Some("1.0.0"),
            "/repo/pkg/package.json",
            "pkg/package.json",
            Language::Node,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("pkg/package.json"),
            (UpdateType::Minor, vec![]),
        );

        let result = version_display_with_update(&project, repo_root, &update_map).unwrap();
        assert_eq!(result, "v1.0.0 → v1.1.0");
    }

    #[test]
    fn test_version_display_with_update_key_hit_major() {
        // Verify the display format works for other update types too
        let pkg = MockPackage::with_all(
            Some("my-pkg"),
            Some("1.0.0"),
            "/repo/pkg/package.json",
            "pkg/package.json",
            Language::Node,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("pkg/package.json"),
            (UpdateType::Major, vec![]),
        );

        let result = version_display_with_update(&project, repo_root, &update_map).unwrap();
        assert_eq!(result, "v1.0.0 → v2.0.0");
    }

    #[test]
    fn test_version_display_with_update_key_hit_patch() {
        // Verify the display format works for patch updates
        let pkg = MockPackage::with_all(
            Some("my-pkg"),
            Some("1.0.0"),
            "/repo/pkg/package.json",
            "pkg/package.json",
            Language::Node,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("pkg/package.json"),
            (UpdateType::Patch, vec![]),
        );

        let result = version_display_with_update(&project, repo_root, &update_map).unwrap();
        assert_eq!(result, "v1.0.0 → v1.0.1");
    }

    #[test]
    fn test_version_display_with_update_path_outside_repo_root() {
        // Case (d): project path outside repo_root with a NON-EMPTY map
        // → Err (from get_relative_path_ref)
        let pkg = MockPackage::with_all(
            Some("my-pkg"),
            Some("1.0.0"),
            "/other/pkg/package.json",
            "pkg/package.json",
            Language::Node,
        );
        let project = Project::Package(Box::new(pkg));
        let repo_root = Path::new("/repo");
        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("pkg/package.json"),
            (UpdateType::Minor, vec![]),
        );

        let result = version_display_with_update(&project, repo_root, &update_map);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not within"));
    }
}
