use napi::Result;
use napi_derive::napi;

#[napi]
pub async fn run(_args: Vec<String>) -> Result<()> {
  // cli::main(args.as_slice())
  //   .await
  //   .map_err(|e| napi::Error::from_reason(e.to_string()))
  Ok(())
}
