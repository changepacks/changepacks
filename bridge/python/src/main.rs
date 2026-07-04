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
        // Exit gracefully on user cancellation (Ctrl+C or ESC), mirroring
        // `bridge/node/src/lib.rs` and `crates/changepacks/src/main.rs`.
        if e.downcast_ref::<changepacks_cli::UserCancelled>().is_some() {
            std::process::exit(0);
        }
        return Err(e);
    }
    Ok(())
}
