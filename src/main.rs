mod archive;
mod asr;
mod audio;
mod cli;
mod config;
mod diarize;
mod export;
mod logging;
mod models;
mod paths;
mod pipeline;
mod tui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
