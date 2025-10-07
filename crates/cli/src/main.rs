use clap::{Parser, Subcommand};
mod check;
mod init;

#[derive(Parser, Debug)]
#[command(author, version, about = "Changepack CLI")]
struct Cli {
    /// Path to the project directory
    pub path: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}
#[derive(Subcommand, Debug)]
enum Commands {
    Init(init::InitArgs),
    Check(check::CheckArgs),
}

fn main() {
    let cli = Cli::parse();
    if let Some(command) = cli.command {
        match command {
            Commands::Init(args) => init::handle_init(&args),
            Commands::Check(args) => check::handle_check(&args),
        }
    } else {
        // collect all projects
        if let Some(path) = &cli.path {
            println!("Collecting projects from: {}", path);
        } else {
            println!("Collecting projects from current directory");
        }
    }
}
