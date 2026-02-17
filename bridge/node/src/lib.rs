//! # changepacks-node-bridge
//!
//! N-API FFI bindings for npm distribution of changepacks.
//!
//! Wraps the changepacks CLI as an async N-API function callable from Node.js. Built
//! with napi-rs to produce native modules for `x86_64` and `aarch64` targets on Windows,
//! macOS, and Linux.

use napi::{Error, Result};
use napi_derive::napi;

#[napi]
/// # Errors
///
/// Returns an error if the CLI command execution fails.
#[cfg(not(tarpaulin_include))]
pub async fn main() -> Result<()> {
  changepacks_cli::main(&std::env::args().skip(1).collect::<Vec<String>>())
    .await
    .map_err(|e| {
      if e.downcast_ref::<changepacks_cli::UserCancelled>().is_some() {
        std::process::exit(0);
      }
      Error::from_reason(e.to_string())
    })
}
