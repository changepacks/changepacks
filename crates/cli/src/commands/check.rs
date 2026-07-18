use changepacks_core::{ChangePackResultLog, Project, UpdateType, normalize_path_separators};

use anyhow::Result;
use changepacks_utils::{
    apply_reverse_dependencies, display_update, gen_changepack_result_map, gen_update_map,
    get_relative_path_ref,
};
use clap::Args;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::io::Write;
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

    /// Display projects as a dependency tree (currently supports stdout output only).
    #[arg(long)]
    tree: bool,

    /// Filter projects by language. Can be specified multiple times to include multiple languages.
    #[arg(short, long, value_enum)]
    pub language: Vec<CliLanguage>,
}

fn validate_check_args(args: &CheckArgs) -> Result<()> {
    if args.tree && matches!(args.format, FormatOptions::Json) {
        anyhow::bail!(
            "`--tree` currently supports stdout output only; remove `--format json` or use `--format stdout`"
        );
    }

    Ok(())
}

/// Check project status
///
/// # Errors
/// Returns error if arguments are incompatible, command context creation, or project checking fails.
///
pub async fn handle_check(args: &CheckArgs) -> Result<()> {
    validate_check_args(args)?;

    let ctx = CommandContext::new(args.remote).await?;

    let mut projects = collect_projects(&ctx.project_finders);
    let mut update_map = gen_update_map(&ctx.changepacks_dir, &ctx.config).await?;

    // Expand over the full project graph before filtering output, matching update.
    apply_reverse_dependencies(&mut update_map, &projects, &ctx.repo_root_path)?;

    if let Some(filter) = &args.filter {
        projects.retain(|p| filter.matches(p));
    }
    retain_by_language(&args.language, &mut projects);
    projects.sort();
    if let FormatOptions::Stdout = args.format {
        println!("Found {} projects", projects.len());
    }

    if args.tree {
        // Tree mode: show dependencies as a tree
        let stdout = std::io::stdout();
        display_tree(
            &projects,
            &ctx.repo_root_path,
            &update_map,
            &mut stdout.lock(),
        )?;
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

/// Resolve and sort a project's monorepo-local dependencies.
///
/// Unknown dependency names are external and ignored. A local name must map
/// to exactly one manifest; ambiguous names are rejected before rendering.
enum NameIndexEntry<'a> {
    Unique(&'a Project),
    Ambiguous(Vec<&'a Project>),
}

type NameIndex<'a> = HashMap<&'a str, NameIndexEntry<'a>>;
type ResolvedDeps<'a> = HashMap<&'a Path, Vec<(&'a str, &'a Project)>>;

fn resolved_monorepo_deps<'a>(
    project: &'a Project,
    name_index: &NameIndex<'a>,
) -> Result<Vec<(&'a str, &'a Project)>> {
    let mut deps: Vec<&str> = project.dependencies().iter().map(String::as_str).collect();
    deps.sort_unstable();
    let mut resolved = Vec::with_capacity(deps.len());
    for dep in deps {
        match name_index.get(dep) {
            Some(NameIndexEntry::Unique(dep_project)) => {
                resolved.push((dep, *dep_project));
            }
            Some(NameIndexEntry::Ambiguous(candidates)) => {
                let mut paths: Vec<String> = candidates
                    .iter()
                    .map(|candidate| {
                        normalize_path_separators(&candidate.relative_path().to_string_lossy())
                    })
                    .collect();
                paths.sort_unstable();
                anyhow::bail!(
                    "dependency `{dep}` is ambiguous; candidate manifests: {}",
                    paths.join(", ")
                );
            }
            None => {}
        }
    }
    Ok(resolved)
}

/// Display projects as a dependency tree
///
fn display_tree(
    projects: &[&Project],
    repo_root_path: &std::path::Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    writer: &mut impl Write,
) -> Result<()> {
    let mut name_index = NameIndex::with_capacity(projects.len());
    for project in projects {
        let Some(name) = project.name() else {
            continue;
        };
        match name_index.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(NameIndexEntry::Unique(project));
            }
            Entry::Occupied(mut entry) => match entry.get_mut() {
                NameIndexEntry::Unique(existing) => {
                    let existing = *existing;
                    entry.insert(NameIndexEntry::Ambiguous(vec![existing, project]));
                }
                NameIndexEntry::Ambiguous(candidates) => candidates.push(project),
            },
        }
    }

    let mut resolved_deps = ResolvedDeps::with_capacity(projects.len());
    for project in projects {
        // Resolve every edge before output so ambiguity never produces a partial tree.
        resolved_deps.insert(
            project.path(),
            resolved_monorepo_deps(project, &name_index)?,
        );
    }

    // Manifest paths keep same-named projects distinct in root detection.
    let has_dependents_cap: usize = resolved_deps.values().map(Vec::len).sum();
    let mut has_dependents: HashSet<&Path> = HashSet::with_capacity(has_dependents_cap);
    has_dependents.extend(
        resolved_deps
            .values()
            .flatten()
            .map(|(_, project)| project.path()),
    );

    // Root order remains name-first; manifest path breaks ties for duplicates.
    let mut sorted_roots: Vec<&Project> = Vec::with_capacity(projects.len());
    for project in projects {
        if !has_dependents.contains(project.path()) {
            sorted_roots.push(project);
        }
    }
    sorted_roots.sort_unstable_by(|left, right| {
        left.name_or_noname()
            .cmp(right.name_or_noname())
            .then_with(|| left.relative_path().cmp(right.relative_path()))
    });

    // Display tree starting from roots.
    let mut visited: HashSet<&Path> = HashSet::with_capacity(projects.len());
    let mut ctx = TreeContext {
        resolved_deps: &resolved_deps,
        repo_root_path,
        update_map,
        line_cache: HashMap::with_capacity(projects.len()),
    };
    for (idx, root) in sorted_roots.iter().enumerate() {
        let is_last = idx == sorted_roots.len() - 1;
        display_tree_node(root, &mut ctx, "", is_last, &mut visited, writer)?;
    }

    // Display projects that weren't part of the tree (for example rootless cycles).
    for project in projects {
        if !ctx.line_cache.contains_key(project.path()) {
            writeln!(writer, "{}", cached_project_line(project, &mut ctx)?)?;
        }
    }

    Ok(())
}

/// Context for tree display operations
struct TreeContext<'a> {
    resolved_deps: &'a ResolvedDeps<'a>,
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
                ctx.resolved_deps,
            )?;
            Ok(entry.insert(line).as_str())
        }
    }
}

enum TreeFrame<'a> {
    Node {
        project: &'a Project,
        prefix: String,
        is_last: bool,
    },
    Dependencies {
        project: &'a Project,
        prefix: String,
        next_index: usize,
    },
}

/// Display a single node and its dependencies without growing the call stack.
fn display_tree_node<'a>(
    project: &'a Project,
    ctx: &mut TreeContext<'a>,
    prefix: &str,
    is_last: bool,
    visited: &mut HashSet<&'a Path>,
    writer: &mut impl Write,
) -> Result<()> {
    let mut frames = vec![TreeFrame::Node {
        project,
        prefix: prefix.to_owned(),
        is_last,
    }];

    while let Some(frame) = frames.pop() {
        match frame {
            TreeFrame::Node {
                project,
                prefix,
                is_last,
            } => {
                let project_path = project.path();
                if visited.insert(project_path) {
                    writeln!(
                        writer,
                        "{}{}{}",
                        prefix,
                        tree_connector(is_last),
                        cached_project_line(project, ctx)?
                    )?;
                }

                if ctx
                    .resolved_deps
                    .get(project_path)
                    .is_some_and(|deps| !deps.is_empty())
                {
                    frames.push(TreeFrame::Dependencies {
                        project,
                        prefix: format!("{}{}", prefix, if is_last { "    " } else { "│   " }),
                        next_index: 0,
                    });
                }
            }
            TreeFrame::Dependencies {
                project,
                prefix,
                next_index,
            } => {
                let Some(deps) = ctx.resolved_deps.get(project.path()) else {
                    continue;
                };
                let Some(dep_project) = deps.get(next_index).map(|&(_, project)| project) else {
                    continue;
                };
                let is_last_dep = next_index == deps.len() - 1;

                if !is_last_dep {
                    frames.push(TreeFrame::Dependencies {
                        project,
                        prefix: prefix.clone(),
                        next_index: next_index + 1,
                    });
                }

                if visited.contains(dep_project.path()) {
                    writeln!(
                        writer,
                        "{}{}{}",
                        prefix,
                        tree_connector(is_last_dep),
                        cached_project_line(dep_project, ctx)?
                    )?;
                } else {
                    frames.push(TreeFrame::Node {
                        project: dep_project,
                        prefix,
                        is_last: is_last_dep,
                    });
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
fn format_project_line(
    project: &Project,
    repo_root_path: &std::path::Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
    resolved_deps: &ResolvedDeps<'_>,
) -> Result<String> {
    use colored::Colorize;

    let version = version_display_with_update(project, repo_root_path, update_map)?;

    let changed_marker = changed_marker(project);

    // Reuse the precomputed sorted dependencies so graph construction remains
    // the only resolution and ambiguity pass.
    let filtered_deps = resolved_deps
        .get(project.path())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut deps_str =
        String::with_capacity(filtered_deps.iter().map(|(dep, _)| dep.len() + 9).sum());
    for (dep, _) in filtered_deps {
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

    #[test]
    fn test_check_args_tree_with_stdout_is_valid() {
        let cli = TestCli::parse_from(["test", "--tree", "--format", "stdout"]);

        assert!(validate_check_args(&cli.check).is_ok());
    }

    #[test]
    fn test_check_args_json_without_tree_is_valid() {
        let cli = TestCli::parse_from(["test", "--format", "json"]);

        assert!(validate_check_args(&cli.check).is_ok());
    }

    #[test]
    fn test_check_args_tree_with_json_is_rejected() {
        let cli = TestCli::parse_from(["test", "--tree", "--format", "json"]);

        let error = validate_check_args(&cli.check).unwrap_err();
        assert_eq!(
            error.to_string(),
            "`--tree` currently supports stdout output only; remove `--format json` or use `--format stdout`"
        );
    }

    #[test]
    fn test_check_args_help_documents_tree_stdout_only() {
        let help = TestCli::try_parse_from(["test", "--help"])
            .err()
            .expect("--help should stop argument parsing")
            .to_string();

        assert!(help.contains("currently supports stdout output only"));
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

    fn package(name: &str, dependencies: &[&str]) -> Project {
        let relative_path = format!("packages/{name}/package.json");
        let path = format!("/repo/{relative_path}");
        let mut package = MockPackage::with_all(
            Some(name),
            Some("1.0.0"),
            &path,
            &relative_path,
            Language::Node,
        );
        package
            .dependencies
            .extend(dependencies.iter().map(ToString::to_string));
        Project::Package(Box::new(package))
    }

    fn try_render_tree(projects: &[&Project]) -> Result<String> {
        let mut output = Vec::new();
        display_tree(projects, Path::new("/repo"), &HashMap::new(), &mut output)?;
        Ok(String::from_utf8(output).unwrap())
    }

    fn render_tree(projects: &[&Project]) -> String {
        try_render_tree(projects).unwrap()
    }

    fn format_project_line_for_test<'a>(
        project: &'a Project,
        repo_root_path: &Path,
        update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>)>,
        name_index: &NameIndex<'a>,
    ) -> Result<String> {
        let mut resolved_deps = ResolvedDeps::with_capacity(1);
        resolved_deps.insert(project.path(), resolved_monorepo_deps(project, name_index)?);
        format_project_line(project, repo_root_path, update_map, &resolved_deps)
    }

    #[test]
    fn test_display_tree_renders_sorted_roots_with_both_connectors() {
        let alpha = package("alpha", &[]);
        let beta = package("beta", &[]);

        assert_eq!(
            render_tree(&[&beta, &alpha]),
            concat!(
                "├── [Node.js] alpha (v1.0.0) - packages/alpha/package.json\n",
                "└── [Node.js] beta (v1.0.0) - packages/beta/package.json\n",
            )
        );
    }

    #[test]
    fn test_display_tree_renders_shared_dependencies_and_visited_branches() {
        let root_a = package("root-a", &["shared"]);
        let root_b = package("root-b", &["shared", "z-last"]);
        let shared = package("shared", &[]);
        let z_last = package("z-last", &[]);

        assert_eq!(
            render_tree(&[&z_last, &shared, &root_b, &root_a]),
            concat!(
                "├── [Node.js] root-a (v1.0.0) - packages/root-a/package.json [deps:\n",
                "        shared]\n",
                "│   └── [Node.js] shared (v1.0.0) - packages/shared/package.json\n",
                "└── [Node.js] root-b (v1.0.0) - packages/root-b/package.json [deps:\n",
                "        shared\n",
                "        z-last]\n",
                "    ├── [Node.js] shared (v1.0.0) - packages/shared/package.json\n",
                "    └── [Node.js] z-last (v1.0.0) - packages/z-last/package.json\n",
            )
        );
    }

    #[test]
    fn test_display_tree_stops_at_cycle_visited_node() {
        let root = package("root", &["a"]);
        let a = package("a", &["b"]);
        let b = package("b", &["a"]);

        assert_eq!(
            render_tree(&[&b, &root, &a]),
            concat!(
                "└── [Node.js] root (v1.0.0) - packages/root/package.json [deps:\n",
                "        a]\n",
                "    └── [Node.js] a (v1.0.0) - packages/a/package.json [deps:\n",
                "        b]\n",
                "        └── [Node.js] b (v1.0.0) - packages/b/package.json [deps:\n",
                "        a]\n",
                "            └── [Node.js] a (v1.0.0) - packages/a/package.json [deps:\n",
                "        b]\n",
            )
        );
    }

    #[test]
    fn test_display_tree_renders_rootless_cycle_as_orphans() {
        let a = package("a", &["b"]);
        let b = package("b", &["a"]);

        assert_eq!(
            render_tree(&[&a, &b]),
            concat!(
                "[Node.js] a (v1.0.0) - packages/a/package.json [deps:\n",
                "        b]\n",
                "[Node.js] b (v1.0.0) - packages/b/package.json [deps:\n",
                "        a]\n",
            )
        );
    }

    #[test]
    fn test_display_tree_handles_deep_acyclic_chain_without_stack_growth() {
        const DEPTH: usize = 10_000;

        let names: Vec<String> = (0..DEPTH).map(|index| format!("node-{index:05}")).collect();
        let projects: Vec<Project> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let dependencies: Vec<&str> = names
                    .get(index + 1)
                    .map(String::as_str)
                    .into_iter()
                    .collect();
                package(name, &dependencies)
            })
            .collect();
        let project_refs: Vec<&Project> = projects.iter().collect();

        assert!(
            display_tree(
                &project_refs,
                Path::new("/repo"),
                &HashMap::new(),
                &mut std::io::sink(),
            )
            .is_ok()
        );
    }

    #[test]
    fn test_display_tree_rejects_referenced_duplicate_name_with_sorted_manifest_paths() {
        let app = package("app", &["shared"]);
        let shared_z = package("shared", &[]);
        let shared_a = Project::Package(Box::new(MockPackage::with_all(
            Some("shared"),
            Some("2.0.0"),
            "/repo/crates/shared/Cargo.toml",
            "crates/shared/Cargo.toml",
            Language::Rust,
        )));

        let error = try_render_tree(&[&shared_z, &app, &shared_a]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "dependency `shared` is ambiguous; candidate manifests: crates/shared/Cargo.toml, packages/shared/package.json"
        );
    }

    #[test]
    fn test_display_tree_selects_first_ambiguous_dependency_deterministically() {
        let alpha_node = package("alpha", &[]);
        let alpha_rust = Project::Package(Box::new(MockPackage::with_all(
            Some("alpha"),
            Some("2.0.0"),
            "/repo/crates/alpha/Cargo.toml",
            "crates/alpha/Cargo.toml",
            Language::Rust,
        )));
        let zeta_node = package("zeta", &[]);
        let zeta_rust = Project::Package(Box::new(MockPackage::with_all(
            Some("zeta"),
            Some("2.0.0"),
            "/repo/crates/zeta/Cargo.toml",
            "crates/zeta/Cargo.toml",
            Language::Rust,
        )));

        for _ in 0..64 {
            let app = package("app", &["zeta", "alpha"]);
            let error = try_render_tree(&[&app, &zeta_node, &alpha_rust, &alpha_node, &zeta_rust])
                .unwrap_err();

            assert_eq!(
                error.to_string(),
                "dependency `alpha` is ambiguous; candidate manifests: crates/alpha/Cargo.toml, packages/alpha/package.json"
            );
        }
    }

    #[test]
    fn test_display_tree_resolves_real_noname_without_unnamed_manifest_collision() {
        let app = package("app", &["noname"]);
        let real_noname = package("noname", &[]);
        let unnamed = Project::Package(Box::new(MockPackage::with_all(
            None,
            Some("1.0.0"),
            "/repo/packages/unnamed/package.json",
            "packages/unnamed/package.json",
            Language::Node,
        )));

        assert_eq!(
            render_tree(&[&unnamed, &real_noname, &app]),
            concat!(
                "├── [Node.js] app (v1.0.0) - packages/app/package.json [deps:\n",
                "        noname]\n",
                "│   └── [Node.js] noname (v1.0.0) - packages/noname/package.json\n",
                "└── [Node.js] noname (v1.0.0) - packages/unnamed/package.json\n",
            )
        );
    }

    #[test]
    fn test_display_tree_does_not_resolve_multiple_unnamed_manifests_as_noname() {
        let app = package("app", &["noname"]);
        let unnamed_a = Project::Package(Box::new(MockPackage::with_all(
            None,
            Some("1.0.0"),
            "/repo/packages/a/package.json",
            "packages/a/package.json",
            Language::Node,
        )));
        let unnamed_z = Project::Package(Box::new(MockPackage::with_all(
            None,
            Some("1.0.0"),
            "/repo/packages/z/package.json",
            "packages/z/package.json",
            Language::Node,
        )));

        assert_eq!(
            render_tree(&[&unnamed_z, &app, &unnamed_a]),
            concat!(
                "├── [Node.js] app (v1.0.0) - packages/app/package.json\n",
                "├── [Node.js] noname (v1.0.0) - packages/a/package.json\n",
                "└── [Node.js] noname (v1.0.0) - packages/z/package.json\n",
            )
        );
    }

    #[test]
    fn test_display_tree_renders_every_unreferenced_duplicate_by_manifest_path() {
        let node_core = package("core", &[]);
        let rust_core = Project::Package(Box::new(MockPackage::with_all(
            Some("core"),
            Some("2.0.0"),
            "/repo/crates/core/Cargo.toml",
            "crates/core/Cargo.toml",
            Language::Rust,
        )));

        assert_eq!(
            render_tree(&[&node_core, &rust_core]),
            concat!(
                "├── [Rust] core (v2.0.0) - crates/core/Cargo.toml\n",
                "└── [Node.js] core (v1.0.0) - packages/core/package.json\n",
            )
        );
    }

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
        let mut name_to_project = NameIndex::new();
        name_to_project.insert("my-lib", NameIndexEntry::Unique(&project));

        let line = format_project_line_for_test(&project, repo_root, &update_map, &name_to_project)
            .unwrap();
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
        let mut name_to_project = NameIndex::new();
        name_to_project.insert("my-workspace", NameIndexEntry::Unique(&project));

        let line = format_project_line_for_test(&project, repo_root, &update_map, &name_to_project)
            .unwrap();
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
        let name_to_project = NameIndex::new();

        let line = format_project_line_for_test(&project, repo_root, &update_map, &name_to_project)
            .unwrap();
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
        let name_to_project = NameIndex::new();

        let line = format_project_line_for_test(&project, repo_root, &update_map, &name_to_project)
            .unwrap();
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
        let mut name_to_project = NameIndex::new();
        name_to_project.insert("app", NameIndexEntry::Unique(&project));
        name_to_project.insert("core-lib", NameIndexEntry::Unique(&dep_project));

        let line = format_project_line_for_test(&project, repo_root, &update_map, &name_to_project)
            .unwrap();
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
        let name_to_project = NameIndex::new();

        let line = format_project_line_for_test(&project, repo_root, &update_map, &name_to_project)
            .unwrap();
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
        let mut name_to_project = NameIndex::new();
        name_to_project.insert("app", NameIndexEntry::Unique(&project));
        name_to_project.insert("apple", NameIndexEntry::Unique(&apple_project));
        name_to_project.insert("zebra", NameIndexEntry::Unique(&zebra_project));
        name_to_project.insert("mango", NameIndexEntry::Unique(&mango_project));

        let line = format_project_line_for_test(&project, repo_root, &update_map, &name_to_project)
            .unwrap();
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
        let resolved_deps = ResolvedDeps::new();

        // Create a TreeContext with an empty line_cache
        let mut ctx = TreeContext {
            resolved_deps: &resolved_deps,
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
