#[tokio::main]
async fn main() -> anyhow::Result<()> {
    changepacks_cli::main(&std::env::args().collect::<Vec<String>>()).await
}
