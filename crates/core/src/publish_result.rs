use serde::Serialize;

#[derive(Serialize, Debug)]
pub struct PublishResult {
    result: bool,
    error: Option<String>,
}

impl PublishResult {
    pub const fn new(result: bool, error: Option<String>) -> Self {
        Self { result, error }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_result_new_success() {
        let result = PublishResult::new(true, None);
        assert!(result.result);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_publish_result_new_failure() {
        let result = PublishResult::new(false, Some("Error message".to_string()));
        assert!(!result.result);
        assert_eq!(result.error, Some("Error message".to_string()));
    }

    #[test]
    fn test_publish_result_debug() {
        let result = PublishResult::new(true, None);
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("PublishResult"));
    }
}
