use changepacks_core::Project;

use anyhow::Result;
use changepacks_utils::gen_update_map;
use clap::Args;
use std::io::Write;

use crate::{
    CommandContext,
    commands::{
        changepack_result_json,
        tree::{display_tree, version_display_with_update},
    },
    finders::collect_projects,
    options::{CliLanguage, FilterOptions, FormatOptions, retain_by_filters},
};

/// Format the "(changed)" marker for a project, colored bright yellow if changed.
///
/// Shared with the `--tree` renderer in `commands::tree`, which appends the same
/// marker to every tree line.
pub(super) fn changed_marker(project: &Project) -> colored::ColoredString {
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
    update_map.apply_reverse_dependencies(&projects, &ctx.repo_root_path)?;

    retain_by_filters(&mut projects, args.filter.as_ref(), &args.language);
    projects.sort();
    // One stdout lock for the whole render: `println!` re-acquires the global
    // lock per line and panics on a write failure (a broken pipe from
    // `changepacks check | head`), while a held `StdoutLock` writes through the
    // same `LineWriter` and lets an io error propagate as a typed error.
    let mut out = std::io::stdout().lock();

    if let FormatOptions::Stdout = args.format {
        writeln!(out, "Found {} projects", projects.len())?;
    }

    if args.tree {
        // Tree mode: show dependencies as a tree
        display_tree(&projects, &ctx.repo_root_path, &update_map, &mut out)?;
    } else {
        match args.format {
            FormatOptions::Stdout => {
                for project in projects {
                    let changed_marker = changed_marker(project);
                    let version_str =
                        version_display_with_update(project, &ctx.repo_root_path, &update_map)?;
                    writeln!(
                        out,
                        "{}{}",
                        project.format_line(Some(&version_str)),
                        changed_marker
                    )?;
                }
            }
            FormatOptions::Json => {
                let json =
                    changepack_result_json(projects.as_slice(), &ctx.repo_root_path, &update_map)?;
                writeln!(out, "{json}")?;
            }
        }
    }
    Ok(())
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
}
