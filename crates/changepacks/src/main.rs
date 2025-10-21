use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    cli::main(std::env::args().collect::<Vec<String>>().as_slice()).await
}
