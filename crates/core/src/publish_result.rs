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
    pub const fn new(result: bool, error: Option<String>, stdout: String, stderr: String) -> Self {
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
    use serde_json::Value;

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
        let debug_str = format!("{result:?}");
        assert!(debug_str.contains("PublishResult"));
    }

    /// `PublishResult` is the payload of `publish --format json`, a hard backward-compatible
    /// surface. The key names must stay `snake_case` (no `rename_all`) and `error` must always
    /// be emitted, serializing as JSON `null` on success rather than being skipped.
    #[test]
    fn test_publish_result_serialize_json_contract() {
        let success: Value =
            serde_json::to_value(PublishResult::new(true, None, "out".into(), String::new()))
                .unwrap();
        let success_object = success.as_object().unwrap();

        assert_eq!(success_object.len(), 4);
        assert_eq!(success_object.get("result"), Some(&Value::Bool(true)));
        assert_eq!(success_object.get("error"), Some(&Value::Null));
        assert_eq!(
            success_object.get("stdout"),
            Some(&Value::String("out".to_string()))
        );
        assert_eq!(
            success_object.get("stderr"),
            Some(&Value::String(String::new()))
        );

        let failure: Value = serde_json::to_value(PublishResult::new(
            false,
            Some("boom".into()),
            String::new(),
            "err".into(),
        ))
        .unwrap();
        let failure_object = failure.as_object().unwrap();

        assert_eq!(failure_object.len(), 4);
        assert_eq!(failure_object.get("result"), Some(&Value::Bool(false)));
        assert_eq!(
            failure_object.get("error"),
            Some(&Value::String("boom".to_string()))
        );
        assert_eq!(
            failure_object.get("stdout"),
            Some(&Value::String(String::new()))
        );
        assert_eq!(
            failure_object.get("stderr"),
            Some(&Value::String("err".to_string()))
        );
    }
}
