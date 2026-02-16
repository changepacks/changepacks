use clap::ValueEnum;

/// CLI output format selection.
///
/// Controls whether commands print human-readable output or JSON for CI integration.
#[derive(Debug, Clone, ValueEnum)]
pub enum FormatOptions {
    /// JSON format for CI/CD pipelines
    #[value(name = "json")]
    Json,
    /// Human-readable colored terminal output
    #[value(name = "stdout")]
    Stdout,
}

impl FormatOptions {
    pub fn print(&self, stdout_msg: &str, json_msg: &str) {
        match self {
            Self::Stdout => println!("{stdout_msg}"),
            Self::Json => println!("{json_msg}"),
        }
    }
}
