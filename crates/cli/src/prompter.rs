use anyhow::Result;
use changepacks_core::Project;
use std::fmt::Write as _;
use thiserror::Error;

/// Error type for user cancellation (Ctrl+C or ESC)
#[derive(Debug, Error)]
#[error("operation cancelled by user")]
pub struct UserCancelled;

/// Return `true` when `err` was raised by a graceful user cancellation
/// (Ctrl+C or ESC — anything the prompter maps to [`UserCancelled`]).
///
/// Consolidates the "if the error is a graceful cancellation, exit 0;
/// otherwise propagate" contract shared by three FFI/binary entry
/// points — `bridge/node/src/lib.rs`, `bridge/python/src/main.rs`, and
/// `crates/changepacks/src/main.rs` — each of which previously carried
/// its own inline `err.downcast_ref::<changepacks_cli::UserCancelled>().is_some()`
/// check. Extracting the helper next to `UserCancelled` itself puts the
/// exit-code policy in one place: any future addition of "graceful
/// cancellation" shapes (e.g. an inquire timeout, an SIGTERM handler)
/// lands here and every entry point picks it up automatically.
#[must_use]
pub fn is_user_cancelled(err: &anyhow::Error) -> bool {
    err.downcast_ref::<UserCancelled>().is_some()
}

/// Dependency injection interface for interactive prompts.
///
/// Allows commands to accept `&dyn Prompter` for testability. Production code uses
/// `InquirePrompter`, tests use `MockPrompter` with predetermined responses.
pub trait Prompter: Send + Sync {
    /// # Errors
    /// Returns error if user cancels the selection or interaction fails.
    fn multi_select<'a>(
        &self,
        message: &str,
        options: Vec<&'a Project>,
        defaults: Vec<usize>,
    ) -> Result<Vec<&'a Project>>;

    /// # Errors
    /// Returns error if user cancels the confirmation or interaction fails.
    fn confirm(&self, message: &str) -> Result<bool>;

    /// Confirm `message` unless `skip` short-circuits the prompt to "yes".
    ///
    /// Encodes the `--yes` flag contract shared by `changepacks update` and
    /// `changepacks publish`, each of which previously carried its own inline
    /// `let confirm = if args.yes { true } else { prompter.confirm(msg)? };`.
    /// Keeping the short-circuit in one provided method means the flag can
    /// never drift between commands, and no prompt is issued (nothing is
    /// written to the terminal) when `skip` is `true`.
    ///
    /// # Errors
    /// Returns error if user cancels the confirmation or interaction fails.
    /// Never fails when `skip` is `true`, because no prompt is issued.
    fn confirm_unless(&self, skip: bool, message: &str) -> Result<bool> {
        if skip {
            return Ok(true);
        }
        self.confirm(message)
    }

    /// # Errors
    /// Returns error if user cancels the input or interaction fails.
    fn text(&self, message: &str) -> Result<String>;
}

/// Helper function for handling inquire result errors
fn handle_inquire_result<T>(result: Result<T, inquire::InquireError>) -> Result<T> {
    // Split into separate arms so each branch is on a single line — keeps
    // tarpaulin's per-line attribution accurate on the multi-line `|`
    // pattern that otherwise reports false-negative gaps under normal
    // rustfmt.
    match result {
        Ok(v) => Ok(v),
        Err(inquire::InquireError::OperationCanceled) => Err(UserCancelled.into()),
        Err(inquire::InquireError::OperationInterrupted) => Err(UserCancelled.into()),
        Err(e) => Err(e.into()),
    }
}

/// Score function for project selection: changed projects rank higher in the list.
///
/// Total function — every project maps to a concrete score, so the return type is
/// a bare `i64`. inquire's scorer signal (`Option<i64>`, where `None` filters an
/// option out) is an API-boundary concern wrapped at the closure in `multi_select`,
/// not a property of the domain scorer.
pub(crate) fn score_project(project: &Project) -> i64 {
    if project.is_changed() { 100 } else { 0 }
}

/// Format selected projects as a newline-separated display string.
///
/// Builds the output via a single running `String` (`fmt::Write::write!` per
/// item, `push('\n')` between items) instead of the previous
/// `.map(|p| format!(...)).collect::<Vec<_>>().join("\n")` chain, which
/// allocated N `String`s (one per project), a `Vec<String>` of N entries, and
/// the final joined `String`. Called on every redraw inside the
/// `inquire::MultiSelect` formatter closure, so the savings compound with UI
/// updates.
///
/// Accepts any iterator of project references to avoid materializing a `Vec`
/// in the formatter closure.
pub(crate) fn format_selected_projects<'a>(
    projects: impl IntoIterator<Item = &'a Project>,
) -> String {
    let mut out = String::new();
    for (i, p) in projects.into_iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // `fmt::Write for String` is infallible: its `write_str` only calls
        // `String::push_str` and always returns `Ok(())`. The `expect` documents
        // that invariant instead of silently discarding the `Result`.
        write!(&mut out, "{p}").expect("writing into a String via fmt::Write is infallible");
    }
    out
}

/// Real implementation using inquire crate
#[derive(Default)]
pub struct InquirePrompter;

impl Prompter for InquirePrompter {
    fn multi_select<'a>(
        &self,
        message: &str,
        options: Vec<&'a Project>,
        defaults: Vec<usize>,
    ) -> Result<Vec<&'a Project>> {
        let mut selector = inquire::MultiSelect::new(message, options);
        selector.page_size = 15;
        selector.default = Some(defaults);
        selector.scorer =
            &|_input, option, _string_value, _idx| -> Option<i64> { Some(score_project(option)) };
        selector.formatter = &|option| format_selected_projects(option.iter().map(|o| *o.value));
        handle_inquire_result(selector.prompt())
    }

    fn confirm(&self, message: &str) -> Result<bool> {
        handle_inquire_result(inquire::Confirm::new(message).prompt())
    }

    fn text(&self, message: &str) -> Result<String> {
        handle_inquire_result(inquire::Text::new(message).prompt())
    }
}

/// Mock implementation that returns predefined values (for testing)
pub struct MockPrompter {
    pub select_all: bool,
    pub confirm_value: bool,
    pub text_value: String,
}

impl Default for MockPrompter {
    fn default() -> Self {
        Self {
            select_all: true,
            confirm_value: true,
            text_value: "test note".to_string(),
        }
    }
}

impl Prompter for MockPrompter {
    fn multi_select<'a>(
        &self,
        _message: &str,
        options: Vec<&'a Project>,
        _defaults: Vec<usize>,
    ) -> Result<Vec<&'a Project>> {
        if self.select_all {
            Ok(options)
        } else {
            Ok(vec![])
        }
    }

    fn confirm(&self, _message: &str) -> Result<bool> {
        Ok(self.confirm_value)
    }

    fn text(&self, _message: &str) -> Result<String> {
        Ok(self.text_value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::Language;
    use changepacks_core::test_support::MockPackage;
    use rstest::rstest;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_mock_prompter_default() {
        let prompter = MockPrompter::default();
        assert!(prompter.select_all);
        assert!(prompter.confirm_value);
        assert_eq!(prompter.text_value, "test note");
    }

    #[test]
    fn test_mock_prompter_confirm() {
        let prompter = MockPrompter {
            confirm_value: false,
            ..Default::default()
        };
        assert!(!prompter.confirm("test").unwrap());
    }

    /// Prompter that records how many times `confirm` was invoked, so
    /// `confirm_unless(true, _)` can be asserted to short-circuit *without
    /// prompting* rather than merely returning `true`.
    ///
    /// Uses `AtomicUsize` rather than `Cell<usize>` because `Prompter`
    /// requires `Sync`; the counter is single-threaded in practice, so a
    /// `Mutex` would add locking noise for no benefit.
    struct CountingPrompter {
        confirm_value: bool,
        confirm_calls: AtomicUsize,
    }

    impl Prompter for CountingPrompter {
        fn multi_select<'a>(
            &self,
            _message: &str,
            options: Vec<&'a Project>,
            _defaults: Vec<usize>,
        ) -> Result<Vec<&'a Project>> {
            Ok(options)
        }

        fn confirm(&self, _message: &str) -> Result<bool> {
            self.confirm_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.confirm_value)
        }

        fn text(&self, _message: &str) -> Result<String> {
            Ok(String::new())
        }
    }

    // `skip == true` is the `--yes` path: it must answer "yes" and must never
    // reach `confirm`, because reaching it would draw a prompt in a
    // non-interactive run.
    #[test]
    fn test_confirm_unless_skip_does_not_call_confirm() {
        let prompter = CountingPrompter {
            confirm_value: false,
            confirm_calls: AtomicUsize::new(0),
        };
        assert!(prompter.confirm_unless(true, "message").unwrap());
        assert_eq!(prompter.confirm_calls.load(Ordering::SeqCst), 0);
    }

    // `skip == false` must delegate: exactly one `confirm` call, and the
    // answer is whatever `confirm` returned (both polarities).
    #[rstest]
    #[case(true)]
    #[case(false)]
    fn test_confirm_unless_delegates_to_confirm(#[case] answer: bool) {
        let prompter = CountingPrompter {
            confirm_value: answer,
            confirm_calls: AtomicUsize::new(0),
        };
        assert_eq!(prompter.confirm_unless(false, "message").unwrap(), answer);
        assert_eq!(prompter.confirm_calls.load(Ordering::SeqCst), 1);
    }

    // A failing `confirm` must propagate through `confirm_unless` unchanged —
    // cancellation still downcasts to `UserCancelled`.
    #[test]
    fn test_confirm_unless_propagates_confirm_error() {
        struct CancellingPrompter;

        impl Prompter for CancellingPrompter {
            fn multi_select<'a>(
                &self,
                _message: &str,
                options: Vec<&'a Project>,
                _defaults: Vec<usize>,
            ) -> Result<Vec<&'a Project>> {
                Ok(options)
            }

            fn confirm(&self, _message: &str) -> Result<bool> {
                Err(UserCancelled.into())
            }

            fn text(&self, _message: &str) -> Result<String> {
                Ok(String::new())
            }
        }

        let err = CancellingPrompter
            .confirm_unless(false, "message")
            .unwrap_err();
        assert!(err.downcast_ref::<UserCancelled>().is_some());
        // The skip path never touches `confirm`, so it cannot fail.
        assert!(CancellingPrompter.confirm_unless(true, "message").unwrap());
    }

    #[test]
    fn test_mock_prompter_text() {
        let prompter = MockPrompter {
            text_value: "custom".to_string(),
            ..Default::default()
        };
        assert_eq!(prompter.text("test").unwrap(), "custom");
    }

    #[test]
    fn test_mock_prompter_multi_select_empty() {
        let prompter = MockPrompter {
            select_all: false,
            ..Default::default()
        };
        let options: Vec<&Project> = vec![];
        let result = prompter.multi_select("test", options, vec![]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_handle_inquire_result_ok() {
        let result: Result<&str> = handle_inquire_result(Ok("test_value"));
        assert_eq!(result.unwrap(), "test_value");
    }

    // Cancellation-shaped inquire errors (Ctrl+C, ESC) MUST downcast to
    // `UserCancelled` so callers can distinguish user cancellation from a
    // real interaction failure.
    #[rstest]
    #[case(inquire::InquireError::OperationCanceled)]
    #[case(inquire::InquireError::OperationInterrupted)]
    fn test_handle_inquire_result_cancellation(#[case] err: inquire::InquireError) {
        let result: Result<()> = handle_inquire_result(Err(err));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<UserCancelled>()
                .is_some()
        );
    }

    #[test]
    fn test_handle_inquire_result_other_error() {
        let result: Result<()> = handle_inquire_result(Err(
            inquire::InquireError::InvalidConfiguration("test".into()),
        ));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<UserCancelled>()
                .is_none()
        );
    }

    // Changed projects rank higher (score 100) so they appear at the top
    // of the multi-select list; unchanged ones score 0.
    #[rstest]
    #[case(true, 100)]
    #[case(false, 0)]
    fn test_score_project(#[case] changed: bool, #[case] expected: i64) {
        let mut pkg = MockPackage::with_all(
            Some("pkg"),
            Some("1.0.0"),
            "package.json",
            "package.json",
            Language::Node,
        );
        pkg.is_changed = changed;
        let project = Project::Package(Box::new(pkg));
        assert_eq!(score_project(&project), expected);
    }

    #[test]
    fn test_format_selected_projects_empty() {
        let projects: Vec<&Project> = vec![];
        assert_eq!(format_selected_projects(projects.iter().copied()), "");
    }

    #[test]
    fn test_format_selected_projects_single() {
        let pkg = MockPackage::with_all(
            Some("my-app"),
            Some("1.0.0"),
            "package.json",
            "package.json",
            Language::Node,
        );
        let project = Project::Package(Box::new(pkg));
        let projects = [&project];
        let result = format_selected_projects(projects.iter().copied());
        assert!(result.contains("my-app"));
    }

    #[test]
    fn test_format_selected_projects_multiple() {
        let mut pkg1 = MockPackage::with_all(
            Some("app-a"),
            Some("1.0.0"),
            "package.json",
            "package.json",
            Language::Node,
        );
        pkg1.is_changed = true;
        let p1 = Project::Package(Box::new(pkg1));
        let pkg2 = MockPackage::with_all(
            Some("app-b"),
            Some("1.0.0"),
            "package.json",
            "package.json",
            Language::Node,
        );
        let p2 = Project::Package(Box::new(pkg2));
        let projects = [&p1, &p2];
        let result = format_selected_projects(projects.iter().copied());
        assert!(result.contains('\n'));
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
    }
}
