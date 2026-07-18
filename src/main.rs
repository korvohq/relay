#[tokio::main]
async fn main() {
    if let Err(error) = relay::cli::run().await {
        eprintln!("relay: {error}");
        std::process::exit(1);
    }
}
