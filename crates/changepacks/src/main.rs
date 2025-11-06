use std::process;

#[tokio::main]
async fn main() {
    if let Err(e) =
        changepacks_cli::main(std::env::args().collect::<Vec<String>>().as_slice()).await
    {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
