//! # changepacks
//!
//! Binary entry point for the changepacks CLI tool.
//!
//! Delegates to `changepacks_cli::main()` with command-line arguments. Handles graceful
//! exit on user cancellation (Ctrl+C or ESC) and prints error messages on failure.

use std::process;

#[tokio::main]
async fn main() {
    if let Err(e) =
        changepacks_cli::main(std::env::args().collect::<Vec<String>>().as_slice()).await
    {
        // Exit gracefully on user cancellation (Ctrl+C or ESC)
        if e.downcast_ref::<changepacks_cli::UserCancelled>().is_some() {
            process::exit(0);
        }
        eprintln!("Error: {e}");
        process::exit(1);
    }
}
