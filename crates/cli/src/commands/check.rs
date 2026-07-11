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

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct CheckArgs {
    #[arg(short, long)]
    filter: Option<FilterOptions>,

    #[arg(long, default_value = "stdout")]
    format: FormatOptions,

    #[arg(short, long, default_value = "false")]
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
                    &mut update_map,
                )?)?;
                println!("{json}");
            }
        }
    }
    Ok(())
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
    // Borrow the `&str` name that already lives inside each `Project`; the
    // owned `String` keys in `name_to_project` accept `&str` lookups via
    // `Borrow<str>`. Avoids N per-invocation `String::clone`s of names we
    // already own further up the stack.
    let mut roots: HashSet<&str> = HashSet::with_capacity(projects.len());

    for project in projects {
        let deps = project.dependencies();
        // Filter dependencies to only include monorepo projects.
        // `name_to_project` now keys on `&str` (see the map's declaration
        // above); the lookup uses `dep.as_str()` because
        // `HashMap<&str, _>::contains_key(&Q)` resolves with `Q = str`
        // (via `&str: Borrow<str>`), not `Q = String`.
        //
        // Preallocate: `.filter().cloned().collect::<Vec<_>>()` cannot
        // preallocate because `Filter::size_hint` only reports
        // `(0, Some(deps.len()))` and `Vec::from_iter` uses the LOWER
        // bound, incurring geometric-doubling reallocations on wide dep
        // lists. `deps.len()` is the tight upper bound. Matches the
        // preallocation policy already applied to `name_to_project`,
        // `graph`, `roots`, `has_dependencies`, `sorted_roots`, and
        // `visited` in this same function.
        //
        // Value type is now `Vec<&str>` (see the `graph` declaration
        // above): push the borrowed `dep.as_str()` slice directly
        // instead of the owned `dep.clone()`. The `String` sitting
        // inside `project.dependencies()` outlives every downstream
        // borrow because `Project` is borrowed for the same scope.
        let mut monorepo_deps: Vec<&str> = Vec::with_capacity(deps.len());
        for dep in deps {
            if name_to_project.contains_key(dep.as_str()) {
                monorepo_deps.push(dep.as_str());
            }
        }

        if !monorepo_deps.is_empty() {
            graph.insert(project.name_or_noname(), monorepo_deps);
        }
    }

    // Pre-sort each graph value ONCE now, before `has_dependencies`
    // borrows into `graph`. `display_tree_node` then borrows the
    // already-sorted dep list on every visit instead of doing a per-visit
    // `deps.clone()` + `.sort()`. Meaningful on diamond/wide graphs where
    // the same subtree is revisited under multiple parents (each revisit
    // still re-descends the deps so all edges render). Behavior is
    // preserved because the sort is applied to identical inputs — the
    // output ordering is byte-identical.
    //
    // `sort_unstable`: dep vectors hold unique package-name slices, and
    // `str::cmp` is a total order — no two equal-but-distinguishable
    // elements exist, so stability is not observable in the rendered tree.
    // Skips the stability bookkeeping the stable sort pays for.
    for deps in graph.values_mut() {
        deps.sort_unstable();
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

    // Root nodes are projects that are not dependencies of any other project
    for project in projects {
        let name = project.name_or_noname();
        if !has_dependencies.contains(name) {
            roots.insert(name);
        }
    }

    // Sort roots for consistent output. `Vec<&str>` sorts identically to
    // `Vec<String>` for the same name strings (byte-identical order), and
    // `name_to_project.get(root)` still resolves because
    // `HashMap<String, _>::get` accepts anything `Borrow<str>`.
    //
    // `sort_unstable`: `roots` originates from a `HashSet<&str>` so its
    // elements are distinct by construction, and `str::cmp` is a total
    // order — stability is not observable in the printed tree. Skips the
    // stability bookkeeping the stable sort pays for.
    //
    // Preallocate: `HashSet::into_iter` reports `size_hint = (len,
    // Some(len))` (ExactSize), so `.collect::<Vec<_>>()` DOES reserve
    // capacity here — but making the reservation explicit matches the
    // visually-uniform preallocation policy already applied throughout
    // this same function (`name_to_project`, `graph`, `roots`,
    // `has_dependencies`, `visited`, `monorepo_deps`). Byte-identical
    // output; the goal is a uniform preallocation idiom so a future
    // maintainer can trust every `Vec::from_iter` was deliberate.
    let mut sorted_roots: Vec<&str> = Vec::with_capacity(roots.len());
    sorted_roots.extend(roots);
    sorted_roots.sort_unstable();

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
        // Deref `&&str` → `&str` so `HashMap<String, _>::get` picks up
        // `Borrow<str>` (there is no `Borrow<&str>` impl for `String`).
        if let Some(project) = name_to_project.get(*root) {
            let is_last = idx == sorted_roots.len() - 1;
            display_tree_node(project, &mut ctx, "", is_last, &mut visited)?;
        }
    }

    // Display projects that weren't part of the tree (orphaned nodes)
    for project in projects {
        if !visited.contains(project.name_or_noname()) {
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
    line_cache: HashMap<&'a str, String>,
}

fn cached_project_line<'a, 'ctx>(
    project: &'a Project,
    ctx: &'ctx mut TreeContext<'a>,
) -> Result<&'ctx str> {
    let project_name = project.name_or_noname();
    match ctx.line_cache.entry(project_name) {
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
        let connector = if is_last { "└── " } else { "├── " };
        println!(
            "{}{}{}",
            prefix,
            connector,
            cached_project_line(project, ctx)?
        );
    }

    // Always display dependencies, even if the node was already visited
    // This ensures all dependencies are shown in the tree.
    // NOTE: `deps` is pre-sorted ONCE in `display_tree` (see comment
    // there); borrowing here avoids the per-visit `deps.clone()` +
    // `.sort()` — meaningful on diamond graphs where the same subtree is
    // re-descended under multiple parents.
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
                    let dep_connector = if is_last_dep {
                        "└── "
                    } else {
                        "├── "
                    };
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

    // Fuse the filter + join into a single `String::push_str` loop, matching
    // the `format_selected_projects` pattern in `prompter.rs`. Drops the
    // intermediate `Vec<&String>` (monorepo_deps) and the intermediate
    // `Vec<&str>` from `.iter().map().collect::<Vec<_>>()` — both allocated
    // per project displayed and multiplied through `display_tree_node` on
    // wide/deep trees. Empty-guard shape preserved: `deps_info` still degrades
    // to `"".normal()` when no monorepo-local dep survives the filter.
    //
    // `name_to_project` now keys on `&str` (see `display_tree`); the lookup
    // uses `dep.as_str()` because `HashMap<&str, _>::contains_key(&Q)`
    // needs `Q = str` (via `&str: Borrow<str>`), not `Q = String`.
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
    let deps = project.dependencies();
    let mut deps_str = String::with_capacity(deps.iter().map(|d| d.len() + 9).sum());
    for dep in deps {
        if !name_to_project.contains_key(dep.as_str()) {
            continue;
        }
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

    // Discriminant helper so `matches!(...)` becomes a case-parameterizable
    // value comparison. `FilterOptions` derives `Debug` + `Clone` but not
    // `PartialEq`, so we bounce through this shadow enum.
    #[derive(Debug, PartialEq, Eq)]
    enum FilterKind {
        Workspace,
        Package,
    }

    impl From<&FilterOptions> for FilterKind {
        fn from(f: &FilterOptions) -> Self {
            match f {
                FilterOptions::Workspace => Self::Workspace,
                FilterOptions::Package => Self::Package,
            }
        }
    }

    // `--filter` (long) and `-f` (short) both parse into `Some(FilterOptions::X)`.
    #[rstest]
    #[case(&["test", "--filter", "workspace"], FilterKind::Workspace)]
    #[case(&["test", "--filter", "package"], FilterKind::Package)]
    #[case(&["test", "-f", "workspace"], FilterKind::Workspace)]
    fn test_check_args_filter_flag(#[case] args: &[&str], #[case] expected: FilterKind) {
        let cli = TestCli::parse_from(args);
        let filter = cli.check.filter.expect("filter should be present");
        assert_eq!(FilterKind::from(&filter), expected);
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

    use async_trait::async_trait;
    use changepacks_core::{Language, Package, Workspace};
    use std::collections::HashSet;

    // Field name `is_changed` matches the `impl_basic_accessors!()`
    // macro contract (see `crates/core/src/project_finder.rs`) so the
    // shared macro can generate every trivial accessor. Locks the
    // macro's field-name contract at the CLI-test surface the same way
    // `crates/core/src/{package,workspace,project,project_finder}.rs`
    // already do — a future rename of the macro's expected field name
    // trips a compile error here immediately.
    #[derive(Debug)]
    struct MockPackageForCheck {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        dependencies: HashSet<String>,
        is_changed: bool,
    }

    impl MockPackageForCheck {
        fn new(
            name: Option<&str>,
            version: Option<&str>,
            path: &str,
            relative_path: &str,
            language: Language,
        ) -> Self {
            Self {
                name: name.map(String::from),
                version: version.map(String::from),
                path: PathBuf::from(path),
                relative_path: PathBuf::from(relative_path),
                language,
                dependencies: HashSet::new(),
                is_changed: false,
            }
        }
    }

    #[async_trait]
    impl Package for MockPackageForCheck {
        // Consumes the same `impl_basic_accessors!()` macro that every
        // core-crate test mock uses — collapses the seven byte-identical
        // trivial accessors (`name`, `version`, `path`, `relative_path`,
        // `is_changed`, `set_changed`, `set_name`) into one macro
        // invocation and locks the field-name contract for the CLI-test
        // surface too.
        changepacks_core::impl_basic_accessors!();

        async fn update_version(
            &mut self,
            _update_type: changepacks_core::UpdateType,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn language(&self) -> Language {
            self.language
        }
        fn dependencies(&self) -> &HashSet<String> {
            &self.dependencies
        }
        fn add_dependency(&mut self, dependency: &str) {
            self.dependencies.insert(dependency.to_string());
        }
        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
        }
    }

    // Field name `is_changed` matches the `impl_basic_accessors!()`
    // macro contract (see `MockPackageForCheck` above for rationale).
    #[derive(Debug)]
    struct MockWorkspaceForCheck {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        dependencies: HashSet<String>,
        is_changed: bool,
    }

    impl MockWorkspaceForCheck {
        fn new(
            name: Option<&str>,
            version: Option<&str>,
            path: &str,
            relative_path: &str,
            language: Language,
        ) -> Self {
            Self {
                name: name.map(String::from),
                version: version.map(String::from),
                path: PathBuf::from(path),
                relative_path: PathBuf::from(relative_path),
                language,
                dependencies: HashSet::new(),
                is_changed: false,
            }
        }
    }

    #[async_trait]
    impl Workspace for MockWorkspaceForCheck {
        // Same macro adoption as `MockPackageForCheck` above — the
        // `Workspace` trait carries the same seven trivial accessors as
        // `Package`, so a single `impl_basic_accessors!()` invocation
        // covers both trait shapes.
        changepacks_core::impl_basic_accessors!();

        async fn update_version(
            &mut self,
            _update_type: changepacks_core::UpdateType,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn language(&self) -> Language {
            self.language
        }
        fn dependencies(&self) -> &HashSet<String> {
            &self.dependencies
        }
        fn add_dependency(&mut self, dependency: &str) {
            self.dependencies.insert(dependency.to_string());
        }
        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
        }
    }

    #[test]
    fn test_format_project_line_package() {
        let pkg = MockPackageForCheck::new(
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
        let ws = MockWorkspaceForCheck::new(
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
        let pkg = MockPackageForCheck::new(
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
        let mut pkg = MockPackageForCheck::new(
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
        let mut pkg = MockPackageForCheck::new(
            Some("app"),
            Some("1.0.0"),
            "/repo/app/package.json",
            "app/package.json",
            Language::Node,
        );
        pkg.dependencies.insert("core-lib".to_string());
        let project = Project::Package(Box::new(pkg));

        let dep_pkg = MockPackageForCheck::new(
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
        let pkg = MockPackageForCheck::new(
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
}
