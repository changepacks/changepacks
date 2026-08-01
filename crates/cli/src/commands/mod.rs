mod changepacks;
mod check;
mod config;
mod init;
mod publish;
/// Private `check --tree` renderer; intentionally not re-exported, so the
/// public `commands` surface is unchanged.
mod tree;
mod update;

use std::{
    collections::HashMap,
    hash::BuildHasher,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{ChangePackResultLog, Project, UpdateType};
use changepacks_utils::gen_changepack_result_map;

/// Render the `--format json` changepack-result payload shared by `check` and `update`.
///
/// Both commands must emit byte-identical JSON for the same inputs, so the
/// `gen_changepack_result_map` + `serde_json::to_string_pretty` pair lives here
/// instead of being open-coded twice. Generic over the hasher exactly like
/// `gen_changepack_result_map`, so `UpdatePlan`'s `Deref` target passes through
/// unchanged.
///
/// # Errors
/// Returns an error if building the result map or serializing it fails.
pub(crate) fn changepack_result_json<S: BuildHasher>(
    projects: &[&Project],
    repo_root_path: &Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>), S>,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&gen_changepack_result_map(
        projects,
        repo_root_path,
        update_map,
    )?)?)
}

/// Write one formatted line to a short-lived locked stdout handle.
///
/// `println!` re-acquires the global stdout lock per line and *panics* when the
/// write fails — a broken pipe from `changepacks | head` is a normal way for
/// these commands to end. A short-lived `StdoutLock` writes through the same
/// `LineWriter`, so the bytes are identical, but the io error propagates as a
/// typed error the caller can return. Multi-line renderers (`publish`'s
/// `print_projects_to_publish`, `update`'s `preview_and_confirm`, `check`)
/// deliberately hold one lock across many lines and keep their own handle.
///
/// # Errors
/// Returns the underlying `io::Error` if writing to stdout fails.
pub(crate) fn writeln_stdout(args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
    use std::io::Write as _;

    writeln!(std::io::stdout().lock(), "{args}")
}

/// Write one formatted line to a short-lived locked stderr handle.
///
/// The stderr counterpart of [`writeln_stdout`], and it exists for the same
/// reason: `eprintln!` re-acquires the global stderr lock per line and *panics*
/// when the write fails, which turns a closed stderr (`changepacks publish
/// 2>&1 | head`) into an abort instead of a returnable error. A short-lived
/// `StderrLock` emits the identical bytes — stderr is unbuffered in both cases
/// — while letting the io error propagate as a typed error. `publish`'s
/// `print_publish_output` already applies this policy to the captured child
/// stderr; this helper extends it to the diagnostics the CLI writes itself.
///
/// # Errors
/// Returns the underlying `io::Error` if writing to stderr fails.
pub(crate) fn writeln_stderr(args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
    use std::io::Write as _;

    writeln!(std::io::stderr().lock(), "{args}")
}

/// Join `items` into one `String`, inserting `separator` between elements.
///
/// Several error messages render a list as `a, b, c`. Each site used to
/// hand-roll the same loop — `String::new()`, `enumerate()`, push the separator
/// when the index is non-zero, append the element — with only the per-element
/// rendering differing, which a `Display` bound covers. Accumulating into a
/// single running `String` keeps the allocation profile of those loops
/// (`.map(..).collect::<Vec<String>>().join(..)` allocated one `String` per
/// element plus the `Vec` spine plus the joined `String`), and the one
/// "`fmt::Write for String` is infallible" justification now lives here instead
/// of being restated per call site.
pub(crate) fn join_display<T: std::fmt::Display>(
    items: impl IntoIterator<Item = T>,
    separator: &str,
) -> String {
    use std::fmt::Write as _;

    let mut joined = String::new();
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            joined.push_str(separator);
        }
        // `fmt::Write for String` is infallible: its `write_str` only calls
        // `String::push_str` and always returns `Ok(())`. The `expect` documents
        // that invariant instead of silently discarding the `Result`.
        write!(&mut joined, "{item}").expect("writing into a String via fmt::Write is infallible");
    }
    joined
}

pub use changepacks::ChangepackArgs;
pub use changepacks::handle_changepack;
pub use changepacks::handle_changepack_with_prompter;
pub use check::CheckArgs;
pub use check::handle_check;
pub use config::handle_config;
pub use init::InitArgs;
pub use init::handle_init;
pub use publish::PublishArgs;
pub use publish::handle_publish;
pub use publish::handle_publish_with_prompter;
pub use update::UpdateArgs;
pub use update::handle_update;
pub use update::handle_update_with_prompter;

#[cfg(test)]
mod tests {
    use super::join_display;
    use rstest::rstest;

    /// Table over `join_display`'s separator placement.
    ///
    /// The two production assertions that reach this helper (`update`'s
    /// unresolved-path error and `tree`'s ambiguity error) both pass two or
    /// more non-empty elements, so the empty-iterator and single-element
    /// branches were never executed. The `empty_first_element` case pins that
    /// the separator is gated on the element *index* rather than on the
    /// accumulator being non-empty: with an accumulator-based guard the leading
    /// empty element would silently swallow its separator and yield `"b, c"`.
    #[rstest]
    #[case::empty(&[], ", ", "")]
    #[case::single(&["only"], ", ", "only")]
    #[case::three(&["a", "b", "c"], ", ", "a, b, c")]
    #[case::multi_char_separator(&["apple", "mango"], "\n        ", "apple\n        mango")]
    #[case::empty_first_element(&["", "b", "c"], ", ", ", b, c")]
    #[case::empty_only_element(&[""], ", ", "")]
    fn test_join_display(#[case] items: &[&str], #[case] separator: &str, #[case] expected: &str) {
        assert_eq!(join_display(items.iter(), separator), expected);
    }
}
