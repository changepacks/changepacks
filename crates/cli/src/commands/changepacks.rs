use changepacks_core::{ChangePackLog, Project, UpdateType};
use std::{collections::HashMap, path::PathBuf};
use tokio::fs::write;

use changepacks_utils::get_relative_path;

use anyhow::Result;

use crate::{
    CommandContext,
    finders::collect_projects,
    options::{CliLanguage, FilterOptions, retain_by_language},
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

/// # Errors
/// Returns error if project discovery, prompting, or changepack file creation fails.
///
/// Excluded from coverage: orchestrates `CommandContext::new` (git I/O)
/// and an interactive `prompter.multi_select(...)` flow that needs a real
/// terminal; the underlying helpers are covered separately by their own
/// unit tests.
#[cfg(not(tarpaulin_include))]
pub async fn handle_changepack_with_prompter(
    args: &ChangepackArgs,
    prompter: &dyn Prompter,
) -> Result<()> {
    let ctx = CommandContext::new(args.remote).await?;

    let mut projects = collect_projects(&ctx.project_finders);

    // Hide packages that inherit their version from workspace root.
    // They are updated automatically when the workspace version bumps.
    projects.retain(|p| !matches!(p, Project::Package(pkg) if pkg.inherits_workspace_version()));

    if let Some(filter) = &args.filter {
        projects.retain(|p| filter.matches(p));
    }
    retain_by_language(&args.language, &mut projects);

    println!("Found {} projects", projects.len());
    // workspace first
    projects.sort();

    let mut update_map = HashMap::<PathBuf, UpdateType>::with_capacity(projects.len());

    let update_types: &[UpdateType] = if let Some(update_type) = args.update_type.as_ref() {
        std::slice::from_ref(update_type)
    } else {
        &[UpdateType::Major, UpdateType::Minor, UpdateType::Patch]
    };
    for &update_type in update_types {
        if projects.is_empty() {
            break;
        }

        // Compute relative paths ONCE per update-type turn — up to three
        // turns (Major / Minor / Patch) — so both the "insert into
        // update_map" pass and the "keep_projects" filter reuse them
        // instead of re-allocating a `PathBuf` per (project × pass) pair.
        // The vector is then CONSUMED by the single combined pass below
        // so each entry either moves into `update_map` (selected) or is
        // dropped with the project (not selected) — no clone.
        let rel_paths: Vec<PathBuf> = projects
            .iter()
            .map(|p| get_relative_path(&ctx.repo_root_path, p.path()))
            .collect::<Result<_>>()?;

        let selected_projects = if args.yes {
            projects.clone()
        } else if update_type == UpdateType::Patch && projects.len() == 1 {
            vec![projects[0]]
        } else {
            let message = format!("Select projects to update for {update_type}");
            // Preallocate: `FilterMap`'s `size_hint` reports
            // `(0, Some(projects.len()))` and `Vec::from_iter` reserves
            // against the LOWER bound (0), so a plain `.collect()` here
            // hits geometric-doubling reallocations whenever many projects
            // are marked changed. `projects.len()` is a tight upper bound
            // (each iteration pushes AT MOST one index). Matches the
            // preallocation policy already applied across `sort_by_dep.rs`,
            // `gen_update_map.rs`, `filter_project_dirs.rs`,
            // `apply_reverse_dependencies`, and `check.rs`. Byte-identical
            // output (same indices, same order).
            let mut defaults = Vec::with_capacity(projects.len());
            for (index, project) in projects.iter().enumerate() {
                if project.is_changed() {
                    defaults.push(index);
                }
            }
            prompter.multi_select(&message, projects.clone(), defaults)?
        };

        // Identify selected projects by pointer equality — every entry
        // in `selected_projects` is a copy of the `&Project` reference
        // that already lives in `projects`, so their addresses match and
        // an O(1) HashSet lookup replaces the previous per-project
        // `get_relative_path` recomputation. Then a single combined pass
        // fuses "insert if selected" and "keep if not" so `projects` is
        // walked ONCE per update-type turn instead of twice — and the
        // sort order is preserved because `keep_projects` accumulates in
        // input order, matching the previous filter behaviour byte-for-byte.
        //
        // Preallocate: `HashSet::from_iter` (via `.collect()`) does NOT reserve
        // from `Iterator::size_hint`, so the fill rehashes as it grows — and it
        // runs inside the per-update-type loop (≤3 turns). `selected_projects.len()`
        // is the exact upper bound, so seeding + `.extend(...)` skips those
        // rehashes. Matches the preallocation policy already applied throughout
        // this file (`defaults`, `keep_projects`) and the workspace. Byte-identical
        // pointer-set membership.
        let mut selected_ptrs: std::collections::HashSet<*const Project> =
            std::collections::HashSet::with_capacity(selected_projects.len());
        selected_ptrs.extend(selected_projects.iter().map(|&p| std::ptr::from_ref(p)));

        let mut keep_projects: Vec<_> = Vec::with_capacity(projects.len());
        for (project, rel_path) in projects.iter().copied().zip(rel_paths) {
            if selected_ptrs.contains(&std::ptr::from_ref(project)) {
                update_map.insert(rel_path, update_type);
            } else {
                keep_projects.push(project);
            }
        }
        projects = keep_projects;
    }

    if update_map.is_empty() {
        println!("No projects selected");
        return Ok(());
    }

    let notes = if let Some(message) = &args.message {
        message.clone()
    } else {
        prompter.text("write notes here")?
    };

    if notes.is_empty() {
        println!("Notes are empty");
        return Ok(());
    }
    let changepack_log = ChangePackLog::new(update_map, notes);
    // random uuid
    let changepack_log_id = nanoid::nanoid!();
    let changepack_log_file = ctx
        .changepacks_dir
        .join(format!("changepack_log_{changepack_log_id}.json"));
    write(changepack_log_file, serde_json::to_string(&changepack_log)?).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
