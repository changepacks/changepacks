use std::process;

#[tokio::main]
async fn main() {
    if let Err(e) =
        changepacks_cli::main(std::env::args().collect::<Vec<String>>().as_slice()).await
    {
        // Exit gracefully on user cancellation (Ctrl+C or ESC)
        if e.downcast_ref::<changepacks_cli::UserCancelled>().is_some() {
            process::exit(0);
        }
        eprintln!("Error: {e}");
        process::exit(1);
    }
}
