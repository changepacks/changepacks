use clap::ValueEnum;

#[derive(Debug, Clone, ValueEnum)]
pub enum FormatOptions {
    #[value(name = "json")]
    Json,
    #[value(name = "stdout")]
    Stdout,
}
