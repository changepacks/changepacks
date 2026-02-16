use anyhow::Result;
use changepacks_core::Project;
use thiserror::Error;

/// Error type for user cancellation (Ctrl+C or ESC)
#[derive(Debug, Error)]
#[error("")]
pub struct UserCancelled;

/// Trait for user input prompts - allows dependency injection for testing
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
        Err(inquire::InquireError::OperationCanceled)
        | Err(inquire::InquireError::OperationInterrupted) => Err(UserCancelled.into()),
        Err(e) => Err(e.into()),
    }
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
        selector.scorer = &|_input, option, _string_value, _idx| -> Option<i64> {
            if option.is_changed() {
                Some(100)
            } else {
                Some(0)
            }
        };
        selector.formatter = &|option| {
            option
                .iter()
                .map(|o| format!("{}", o.value))
                .collect::<Vec<_>>()
                .join("\n")
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
}
