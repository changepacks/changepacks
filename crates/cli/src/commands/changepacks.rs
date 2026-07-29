use changepacks_core::{ChangePackLog, Project, UpdateType};
use std::{collections::BTreeMap, path::PathBuf};
use tokio::fs::{create_dir_all, write};

use changepacks_utils::get_relative_path;

use anyhow::{Context, Result};

use crate::{
    CommandContext,
    finders::collect_projects,
    options::{CliLanguage, FilterOptions, retain_by_filters},
    prompter::{InquirePrompter, Prompter},
};

#[derive(Debug)]
pub struct ChangepackArgs {
    pub filter: Option<FilterOptions>,
    pub remote: bool,
    pub yes: bool,
    pub message: Option<String>,
    pub update_type: Option<UpdateType>,
    pub language: Vec<CliLanguage>,
}

/// # Errors
/// Returns error if command context creation or changepack creation fails.
pub async fn handle_changepack(args: &ChangepackArgs) -> Result<()> {
    handle_changepack_with_prompter(args, &InquirePrompter).await
}

/// Select projects and resolve the changepack notes without performing git or file I/O.
///
/// # Errors
/// Returns an error if a project path is outside the repository root or prompting fails.
fn select_changepack(
    mut projects: Vec<&Project>,
    repo_root_path: &std::path::Path,
    args: &ChangepackArgs,
    prompter: &dyn Prompter,
) -> Result<(BTreeMap<PathBuf, UpdateType>, String)> {
    // Hide packages that inherit their version from workspace root.
    // They are updated automatically when the workspace version bumps.
    projects.retain(|p| !matches!(p, Project::Package(pkg) if pkg.inherits_workspace_version()));

    retain_by_filters(&mut projects, args.filter, &args.language);

    println!("Found {} projects", projects.len());
    // workspace first
    projects.sort();

    let mut update_map = BTreeMap::<PathBuf, UpdateType>::new();

    // Compute each project's relative path exactly ONCE per invocation. A
    // project's relative path never changes across update-type turns, so
    // pairing every `&Project` with its `PathBuf` up front — instead of
    // rebuilding `rel_paths` inside the loop (once per surviving project per
    // turn, up to three Major/Minor/Patch turns) — allocates each `PathBuf`
    // a single time. `projects` is already sorted (workspace first) and
    // `into_iter` preserves that order, so `entries` inherits the same
    // display order. Error propagation is unchanged: the first turn
    // previously walked every project in this same sorted order, so any
    // `get_relative_path` failure still surfaces at the same point with the
    // same message.
    let mut entries: Vec<(&Project, PathBuf)> = projects
        .into_iter()
        .map(|p| Ok((p, get_relative_path(repo_root_path, p.path())?)))
        .collect::<Result<_>>()?;

    let update_types: &[UpdateType] = if let Some(update_type) = args.update_type.as_ref() {
        std::slice::from_ref(update_type)
    } else {
        &[UpdateType::Major, UpdateType::Minor, UpdateType::Patch]
    };
    for &update_type in update_types {
        if entries.is_empty() {
            break;
        }

        // Cheap per-turn view of the surviving projects for the prompter /
        // `--yes` / single-patch branches — a pointer collect over `entries`,
        // no path recomputation (each path was allocated once, up front).
        let projects: Vec<&Project> = entries.iter().map(|(p, _)| *p).collect();

        let selected_projects =
            if args.yes || (update_type == UpdateType::Patch && projects.len() == 1) {
                projects
            } else {
                let message = format!("Select projects to update for {update_type}");
                // Preallocate: `FilterMap`'s `size_hint` reports
                // `(0, Some(projects.len()))` and `Vec::from_iter` reserves
                // against the LOWER bound (0), so a plain `.collect()` here
                // hits geometric-doubling reallocations whenever many projects
                // are marked changed. `projects.len()` is a tight upper bound
                // (each iteration pushes AT MOST one index). Matches the
                // preallocation policy already applied across `sort_by_dep.rs`,
                // `gen_update_map.rs`, `find_project_dirs.rs`,
                // `apply_reverse_dependencies`, and `check.rs`. Byte-identical
                // output (same indices, same order).
                let mut defaults = Vec::with_capacity(projects.len());
                for (index, project) in projects.iter().enumerate() {
                    if project.is_changed() {
                        defaults.push(index);
                    }
                }
                prompter.multi_select(&message, projects, defaults)?
            };

        // Identify selected projects by pointer equality — every entry
        // in `selected_projects` is a copy of the `&Project` reference
        // that already lives in `entries`, so their addresses match and
        // an O(1) HashSet lookup drives the combined pass below. That pass
        // fuses "insert if selected" and "keep if not" so `entries` is
        // walked ONCE per update-type turn: selected entries MOVE their
        // `PathBuf` into `update_map` (no clone), unselected entries are
        // kept for the next turn. The sort order is preserved because
        // `keep_entries` accumulates in input order, matching the previous
        // filter behaviour byte-for-byte.
        //
        // Preallocate: `HashSet::from_iter` (via `.collect()`) does NOT reserve
        // from `Iterator::size_hint`, so the fill rehashes as it grows — and it
        // runs inside the per-update-type loop (≤3 turns). `selected_projects.len()`
        // is the exact upper bound, so seeding + `.extend(...)` skips those
        // rehashes. Matches the preallocation policy already applied throughout
        // this file (`defaults`, `keep_entries`) and the workspace. Byte-identical
        // pointer-set membership.
        let mut selected_ptrs: std::collections::HashSet<*const Project> =
            std::collections::HashSet::with_capacity(selected_projects.len());
        selected_ptrs.extend(selected_projects.iter().map(|&p| std::ptr::from_ref(p)));

        let mut keep_entries: Vec<(&Project, PathBuf)> = Vec::with_capacity(entries.len());
        for (project, rel_path) in entries {
            if selected_ptrs.contains(&std::ptr::from_ref(project)) {
                update_map.insert(rel_path, update_type);
            } else {
                keep_entries.push((project, rel_path));
            }
        }
        entries = keep_entries;
    }

    if update_map.is_empty() {
        return Ok((update_map, String::new()));
    }

    let notes = if let Some(message) = &args.message {
        message.clone()
    } else {
        prompter.text("write notes here")?
    };

    Ok((update_map, notes))
}

/// # Errors
/// Returns error if project discovery, prompting, or changepack file creation fails.
pub async fn handle_changepack_with_prompter(
    args: &ChangepackArgs,
    prompter: &dyn Prompter,
) -> Result<()> {
    let ctx = CommandContext::new(args.remote).await?;
    let projects = collect_projects(&ctx.project_finders);
    let (update_map, notes) = select_changepack(projects, &ctx.repo_root_path, args, prompter)?;

    if update_map.is_empty() {
        println!("No projects selected");
        return Ok(());
    }

    if notes.is_empty() {
        println!("Notes are empty");
        return Ok(());
    }
    let changepack_log = ChangePackLog::new(update_map, notes);
    // random nanoid (21-char URL-safe id) for a unique log filename
    let changepack_log_id = nanoid::nanoid!();
    let changepack_log_file = ctx
        .changepacks_dir
        .join(format!("changepack_log_{changepack_log_id}.json"));
    create_dir_all(&ctx.changepacks_dir)
        .await
        .with_context(|| {
            format!(
                "Failed to create changepacks directory {}",
                ctx.changepacks_dir.display()
            )
        })?;
    write(
        &changepack_log_file,
        serde_json::to_string_pretty(&changepack_log)?,
    )
    .await
    .with_context(|| {
        format!(
            "Failed to write changepack log {}",
            changepack_log_file.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompter::MockPrompter;
    use changepacks_core::{
        Language,
        test_support::{MockPackage, MockWorkspace},
    };
    use changepacks_rust::package::RustPackage;

    fn args() -> ChangepackArgs {
        ChangepackArgs {
            filter: None,
            remote: false,
            yes: true,
            message: Some("release note".to_string()),
            update_type: Some(UpdateType::Patch),
            language: vec![],
        }
    }

    fn package(root: &str, name: &str, language: Language) -> Project {
        let path = format!("{root}/{name}/manifest");
        Project::Package(Box::new(MockPackage::with_all(
            Some(name),
            Some("1.0.0"),
            &path,
            &format!("{name}/manifest"),
            language,
        )))
    }

    fn workspace(root: &str, name: &str, language: Language) -> Project {
        let path = format!("{root}/{name}/manifest");
        Project::Workspace(Box::new(MockWorkspace::with_all(
            Some(name),
            Some("1.0.0"),
            &path,
            &format!("{name}/manifest"),
            language,
        )))
    }

    #[test]
    fn select_changepack_applies_project_filter_with_mock_prompter() {
        let root = PathBuf::from("repo");
        let projects = [
            workspace("repo", "workspace", Language::Node),
            package("repo", "package", Language::Node),
        ];
        let mut args = args();
        args.filter = Some(FilterOptions::Package);

        let (updates, message) = select_changepack(
            projects.iter().collect(),
            &root,
            &args,
            &MockPrompter::default(),
        )
        .unwrap();

        assert_eq!(
            updates,
            BTreeMap::from([(PathBuf::from("package/manifest"), UpdateType::Patch)])
        );
        assert_eq!(message, "release note");
    }

    #[test]
    fn select_changepack_applies_language_gate() {
        let root = PathBuf::from("repo");
        let projects = [
            package("repo", "node", Language::Node),
            package("repo", "rust", Language::Rust),
        ];
        let mut args = args();
        args.language = vec![CliLanguage::Rust];

        let (updates, _) = select_changepack(
            projects.iter().collect(),
            &root,
            &args,
            &MockPrompter::default(),
        )
        .unwrap();

        assert_eq!(
            updates,
            BTreeMap::from([(PathBuf::from("rust/manifest"), UpdateType::Patch)])
        );
    }

    #[test]
    fn select_changepack_excludes_rust_packages_with_inherited_versions() {
        let root = PathBuf::from("repo");
        let inherited = Project::Package(Box::new(RustPackage::new_with_workspace_version(
            Some("inherited".to_string()),
            Some("1.0.0".to_string()),
            root.join("inherited/Cargo.toml"),
            PathBuf::from("inherited/Cargo.toml"),
            Some(root.join("Cargo.toml")),
        )));
        let projects = [inherited, package("repo", "owned", Language::Rust)];

        let (updates, _) = select_changepack(
            projects.iter().collect(),
            &root,
            &args(),
            &MockPrompter::default(),
        )
        .unwrap();

        assert_eq!(
            updates,
            BTreeMap::from([(PathBuf::from("owned/manifest"), UpdateType::Patch)])
        );
    }

    #[test]
    fn select_changepack_uses_each_explicit_update_type() {
        let root = PathBuf::from("repo");
        for update_type in [UpdateType::Major, UpdateType::Minor, UpdateType::Patch] {
            let projects = [package("repo", "package", Language::Node)];
            let mut args = args();
            args.update_type = Some(update_type);

            let (updates, _) = select_changepack(
                projects.iter().collect(),
                &root,
                &args,
                &MockPrompter::default(),
            )
            .unwrap();

            assert_eq!(updates.values().copied().collect::<Vec<_>>(), [update_type]);
        }
    }

    #[test]
    fn select_changepack_returns_empty_without_notes_when_nothing_is_selected() {
        let root = PathBuf::from("repo");
        let projects = [
            package("repo", "alpha", Language::Node),
            package("repo", "beta", Language::Node),
        ];
        let mut args = args();
        args.yes = false;
        args.update_type = None;
        args.message = None;
        let prompter = MockPrompter {
            select_all: false,
            text_value: "must not be used".to_string(),
            ..Default::default()
        };

        let (updates, message) =
            select_changepack(projects.iter().collect(), &root, &args, &prompter).unwrap();

        assert!(updates.is_empty());
        assert!(message.is_empty());
    }

    #[test]
    fn select_changepack_returns_updates_in_deterministic_path_order() {
        let root = PathBuf::from("repo");
        let projects = [
            package("repo", "zeta", Language::Node),
            package("repo", "alpha", Language::Node),
            package("repo", "middle", Language::Node),
        ];

        let (updates, _) = select_changepack(
            projects.iter().collect(),
            &root,
            &args(),
            &MockPrompter::default(),
        )
        .unwrap();

        assert_eq!(
            updates.keys().cloned().collect::<Vec<_>>(),
            [
                PathBuf::from("alpha/manifest"),
                PathBuf::from("middle/manifest"),
                PathBuf::from("zeta/manifest"),
            ]
        );
    }

    #[test]
    fn test_changepack_args_debug() {
        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: true,
            message: Some("Test".to_string()),
            update_type: Some(UpdateType::Patch),
            language: vec![],
        };

        // Test Debug trait
        let debug_str = format!("{args:?}");
        assert!(debug_str.contains("ChangepackArgs"));
    }

    #[test]
    fn test_changepack_args_with_filter() {
        let args = ChangepackArgs {
            filter: Some(FilterOptions::Package),
            remote: true,
            yes: false,
            message: None,
            update_type: None,
            language: vec![],
        };

        assert!(args.filter.is_some());
        assert!(args.remote);
        assert!(!args.yes);
        assert!(args.message.is_none());
        assert!(args.update_type.is_none());
    }

    #[test]
    fn test_changepack_args_workspace_filter() {
        let args = ChangepackArgs {
            filter: Some(FilterOptions::Workspace),
            remote: false,
            yes: true,
            message: Some("msg".to_string()),
            update_type: Some(UpdateType::Major),
            language: vec![],
        };

        assert!(matches!(args.filter, Some(FilterOptions::Workspace)));
        assert!(matches!(args.update_type, Some(UpdateType::Major)));
    }

    #[test]
    fn test_changepack_args_minor_update() {
        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: true,
            message: Some("feature".to_string()),
            update_type: Some(UpdateType::Minor),
            language: vec![],
        };

        assert!(matches!(args.update_type, Some(UpdateType::Minor)));
    }

    #[test]
    fn test_changepack_args_with_language() {
        let args = ChangepackArgs {
            filter: None,
            remote: false,
            yes: true,
            message: None,
            update_type: None,
            language: vec![CliLanguage::Node, CliLanguage::Rust],
        };

        assert_eq!(args.language.len(), 2);
    }
}
