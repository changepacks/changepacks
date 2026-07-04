//! # changepacks
//!
//! Binary entry point for the changepacks CLI tool.
//!
//! Delegates to `changepacks_cli::main()` with command-line arguments. Handles graceful
//! exit on user cancellation (Ctrl+C or ESC) and prints error messages on failure.

use std::process;

#[tokio::main]
#[cfg(not(tarpaulin_include))]
async fn main() {
    if let Err(e) =
        changepacks_cli::main(std::env::args().collect::<Vec<String>>().as_slice()).await
    {
        // Consolidated "graceful cancellation → exit(0)" check via the
        // shared `changepacks_cli::is_user_cancelled` helper (Ctrl+C or
        // ESC), mirroring `bridge/node/src/lib.rs` and
        // `bridge/python/src/main.rs`.
        if changepacks_cli::is_user_cancelled(&e) {
            process::exit(0);
        }
        eprintln!("Error: {e}");
        process::exit(1);
    }
}
