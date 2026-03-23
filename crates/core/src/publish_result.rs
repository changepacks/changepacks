use serde::Serialize;

/// Result of a publish attempt with success flag, captured output, and optional error message.
///
/// Used for JSON output format and tracking publish failures across multiple packages.
#[derive(Serialize, Debug)]
pub struct PublishResult {
    /// True if publish succeeded, false otherwise
    result: bool,
    /// Error message if publish failed, None if successful
    error: Option<String>,
    /// Captured stdout from the publish command
    stdout: String,
    /// Captured stderr from the publish command
    stderr: String,
}

impl PublishResult {
    #[must_use]
    pub fn new(result: bool, error: Option<String>, stdout: String, stderr: String) -> Self {
        Self {
            result,
            error,
            stdout,
            stderr,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_result_new_success() {
        let result = PublishResult::new(true, None, "output".into(), String::new());
        assert!(result.result);
        assert!(result.error.is_none());
        assert_eq!(result.stdout, "output");
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn test_publish_result_new_failure() {
        let result = PublishResult::new(
            false,
            Some("Error message".to_string()),
            String::new(),
            "err".into(),
        );
        assert!(!result.result);
        assert_eq!(result.error, Some("Error message".to_string()));
        assert_eq!(result.stderr, "err");
    }

    #[test]
    fn test_publish_result_debug() {
        let result = PublishResult::new(true, None, String::new(), String::new());
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("PublishResult"));
    }
}
