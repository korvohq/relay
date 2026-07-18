// Copyright 2025-present Snab Limited (trading as Korvo)
// SPDX-License-Identifier: Apache-2.0

#[tokio::main]
async fn main() {
    if let Err(error) = relay::cli::run().await {
        eprintln!("relay: {error}");
        std::process::exit(1);
    }
}
