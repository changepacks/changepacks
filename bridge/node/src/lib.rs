use napi::{Error, Result};
use napi_derive::napi;

#[napi]
pub async fn main() -> Result<()> {
  changepacks_cli::main(&std::env::args().skip(1).collect::<Vec<String>>())
    .await
    .map_err(|e| Error::from_reason(e.to_string()))
}
