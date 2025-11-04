use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    changepacks_cli::main(std::env::args().collect::<Vec<String>>().as_slice()).await
}
