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
    changepacks_cli::main(&std::env::args().collect::<Vec<String>>()).await
}
