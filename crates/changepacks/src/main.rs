//! # changepacks
//!
//! Binary entry point for the changepacks CLI tool.
//!
//! Delegates to `changepacks_cli::main()` with command-line arguments. Handles graceful
//! exit on user cancellation (Ctrl+C or ESC) and prints error messages on failure.

#[tokio::main]
#[cfg(not(tarpaulin_include))]
async fn main() {
    if let Err(e) = changepacks_cli::main_from_env(false).await {
        changepacks_cli::exit_if_user_cancelled(&e);
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
