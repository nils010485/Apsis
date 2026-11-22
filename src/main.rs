mod cli;
mod config;
mod connectors;
mod error;
mod jobs;
mod mcp;
mod spillover;

use clap::Parser;

use crate::cli::Cli;
use crate::config::ConfigStore;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let store = match ConfigStore::default_store() {
        Ok(store) => store,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };

    if let Err(err) = cli::run(cli, store).await {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}
