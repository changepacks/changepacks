#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::main(&std::env::args().skip(1).collect::<Vec<String>>()).await
}
