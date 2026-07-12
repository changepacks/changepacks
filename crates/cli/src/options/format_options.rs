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
    pub fn print(self, stdout_msg: &str) {
        match self {
            Self::Stdout => println!("{stdout_msg}"),
            Self::Json => println!("{{}}"),
        }
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
    fn test_format_options_eq() {
        assert_eq!(FormatOptions::Json, FormatOptions::Json);
        assert_eq!(FormatOptions::Stdout, FormatOptions::Stdout);
        assert_ne!(FormatOptions::Json, FormatOptions::Stdout);
    }
}
