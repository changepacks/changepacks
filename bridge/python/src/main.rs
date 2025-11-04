#[tokio::main]
async fn main() -> anyhow::Result<()> {
    changepacks_cli::main(&std::env::args().skip(1).collect::<Vec<String>>()).await
}
