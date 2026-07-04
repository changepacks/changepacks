//! # changepacks-python-bridge
//!
//! Standalone binary for `PyPI` distribution of changepacks.
//!
//! Compiled with maturin as a native executable that can be invoked from Python. The
//! Python stub locates this binary via sysconfig paths and executes it with command-line
//! arguments forwarded from sys.argv.

#[tokio::main]
#[cfg(not(tarpaulin_include))]
async fn main() -> anyhow::Result<()> {
    if let Err(e) = changepacks_cli::main(&std::env::args().collect::<Vec<String>>()).await {
        // Consolidated "graceful cancellation → exit(0)" check via the
        // shared `changepacks_cli::is_user_cancelled` helper — mirroring
        // `bridge/node/src/lib.rs` and `crates/changepacks/src/main.rs`
        // through the same one-liner.
        if changepacks_cli::is_user_cancelled(&e) {
            std::process::exit(0);
        }
        return Err(e);
    }
    Ok(())
}
