use anyhow::Result;
use changepacks_core::Project;
use thiserror::Error;

/// Error type for user cancellation (Ctrl+C or ESC)
#[derive(Debug, Error)]
#[error("")]
pub struct UserCancelled;

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

    /// # Errors
    /// Returns error if user cancels the input or interaction fails.
    fn text(&self, message: &str) -> Result<String>;
}

/// Helper function for handling inquire result errors
fn handle_inquire_result<T>(result: Result<T, inquire::InquireError>) -> Result<T> {
    match result {
        Ok(v) => Ok(v),
        Err(
            inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted,
        ) => Err(UserCancelled.into()),
        Err(e) => Err(e.into()),
    }
}

/// Score function for project selection: changed projects rank higher in the list.
pub(crate) fn score_project(project: &Project) -> Option<i64> {
    if project.is_changed() {
        Some(100)
    } else {
        Some(0)
    }
}

/// Format selected projects as a newline-separated display string.
pub(crate) fn format_selected_projects(projects: &[&Project]) -> String {
    projects
        .iter()
        .map(|p| format!("{p}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Real implementation using inquire crate
#[derive(Default)]
pub struct InquirePrompter;

#[cfg(not(tarpaulin_include))]
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
            &|_input, option, _string_value, _idx| -> Option<i64> { score_project(option) };
        selector.formatter = &|option| {
            let projects: Vec<&Project> = option.iter().map(|o| *o.value).collect();
            format_selected_projects(&projects)
        };
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
    use async_trait::async_trait;
    use changepacks_core::{Language, Package, UpdateType};
    use std::collections::HashSet;
    use std::path::Path;

    /// Minimal mock Package for testing scorer and formatter functions
    #[derive(Debug)]
    struct MockTestPackage {
        name: Option<String>,
        changed: bool,
    }

    impl MockTestPackage {
        fn new(name: &str, changed: bool) -> Self {
            Self {
                name: Some(name.to_string()),
                changed,
            }
        }
    }

    #[async_trait]
    impl Package for MockTestPackage {
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }
        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }
        fn path(&self) -> &Path {
            Path::new("package.json")
        }
        fn relative_path(&self) -> &Path {
            Path::new("package.json")
        }
        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            Ok(())
        }
        fn is_changed(&self) -> bool {
            self.changed
        }
        fn language(&self) -> Language {
            Language::Node
        }
        fn dependencies(&self) -> &HashSet<String> {
            static EMPTY: std::sync::LazyLock<HashSet<String>> =
                std::sync::LazyLock::new(HashSet::new);
            &EMPTY
        }
        fn add_dependency(&mut self, _dep: &str) {}
        fn set_changed(&mut self, changed: bool) {
            self.changed = changed;
        }
        fn default_publish_command(&self) -> String {
            "echo test".to_string()
        }
    }

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

    #[test]
    fn test_handle_inquire_result_operation_canceled() {
        let result: Result<()> =
            handle_inquire_result(Err(inquire::InquireError::OperationCanceled));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<UserCancelled>()
                .is_some()
        );
    }

    #[test]
    fn test_handle_inquire_result_operation_interrupted() {
        let result: Result<()> =
            handle_inquire_result(Err(inquire::InquireError::OperationInterrupted));
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

    #[test]
    fn test_score_project_changed() {
        let project = Project::Package(Box::new(MockTestPackage::new("pkg", true)));
        assert_eq!(score_project(&project), Some(100));
    }

    #[test]
    fn test_score_project_unchanged() {
        let project = Project::Package(Box::new(MockTestPackage::new("pkg", false)));
        assert_eq!(score_project(&project), Some(0));
    }

    #[test]
    fn test_format_selected_projects_empty() {
        let projects: Vec<&Project> = vec![];
        assert_eq!(format_selected_projects(&projects), "");
    }

    #[test]
    fn test_format_selected_projects_single() {
        let project = Project::Package(Box::new(MockTestPackage::new("my-app", false)));
        let projects = vec![&project];
        let result = format_selected_projects(&projects);
        assert!(result.contains("my-app"));
    }

    #[test]
    fn test_format_selected_projects_multiple() {
        let p1 = Project::Package(Box::new(MockTestPackage::new("app-a", true)));
        let p2 = Project::Package(Box::new(MockTestPackage::new("app-b", false)));
        let projects = vec![&p1, &p2];
        let result = format_selected_projects(&projects);
        assert!(result.contains('\n'));
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
    }
}
