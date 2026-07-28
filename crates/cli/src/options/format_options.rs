use clap::ValueEnum;

/// CLI output format selection.
///
/// Controls whether commands print human-readable output or JSON for CI integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FormatOptions {
    /// JSON format for CI/CD pipelines
    #[value(name = "json")]
    Json,
    /// Human-readable colored terminal output
    #[value(name = "stdout")]
    Stdout,
}

impl FormatOptions {
    /// Returns the payload this format prints for `stdout_msg`.
    ///
    /// `Self::Stdout` forwards the human-readable message unchanged, while
    /// `Self::Json` always yields the empty JSON object, ignoring `stdout_msg`.
    #[must_use]
    pub fn message(self, stdout_msg: &str) -> &str {
        match self {
            Self::Stdout => stdout_msg,
            Self::Json => "{}",
        }
    }

    pub fn print(self, stdout_msg: &str) {
        println!("{}", self.message(stdout_msg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn test_format_options_value_enum_json() {
        let format = FormatOptions::from_str("json", true).unwrap();
        assert!(matches!(format, FormatOptions::Json));
    }

    #[test]
    fn test_format_options_value_enum_stdout() {
        let format = FormatOptions::from_str("stdout", true).unwrap();
        assert!(matches!(format, FormatOptions::Stdout));
    }

    #[test]
    fn test_format_options_debug() {
        assert_eq!(format!("{:?}", FormatOptions::Json), "Json");
        assert_eq!(format!("{:?}", FormatOptions::Stdout), "Stdout");
    }

    #[test]
    fn test_format_options_message_stdout_forwards_message() {
        assert_eq!(
            FormatOptions::Stdout.message("No projects to publish"),
            "No projects to publish"
        );
        assert_eq!(FormatOptions::Stdout.message(""), "");
    }

    #[test]
    fn test_format_options_message_json_is_empty_object() {
        assert_eq!(FormatOptions::Json.message("No projects to publish"), "{}");
        assert_eq!(FormatOptions::Json.message("No updates to apply"), "{}");
        assert_eq!(FormatOptions::Json.message(""), "{}");
    }

    #[test]
    fn test_format_options_eq() {
        assert_eq!(FormatOptions::Json, FormatOptions::Json);
        assert_eq!(FormatOptions::Stdout, FormatOptions::Stdout);
        assert_ne!(FormatOptions::Json, FormatOptions::Stdout);
    }
}
