mod asr;
mod audio;
mod cli;
mod config;
mod db;
mod diarize;
mod export;
mod index;
mod llm;
mod logging;
mod paths;
mod pipeline;
mod tui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
