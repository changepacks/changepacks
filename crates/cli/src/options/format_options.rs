use clap::ValueEnum;

#[derive(Debug, Clone, ValueEnum)]
pub enum FormatOptions {
    #[value(name = "json")]
    Json,
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
