#![warn(clippy::pedantic)]

use std::{path::PathBuf, time::Duration};

use clap::{arg, command, Parser};
use process::search_next;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::once::Seen;

mod jellyfin;
mod once;
mod process;
mod sonarr;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Jellyfin baseurl
    #[arg(long, value_name = "URL")]
    jellyfin_url: String,
    /// Jellyfin API key
    #[arg(long, value_name = "API_KEY", env = "JELLYFIN_API_KEY")]
    jellyfin_api_key: String,
    /// Sonarr baseurl
    #[arg(long, value_name = "URL")]
    sonarr_url: String,
    /// Sonarr API key
    #[arg(long, value_name = "API_KEY", env = "SONARR_API_KEY")]
    sonarr_api_key: String,
    /// Polling interval
    #[arg(long, value_name = "SECONDS", default_value_t = 900)]
    interval: u64,
    /// Logging directory
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

pub enum Message {
    NowPlaying(jellyfin::NowPlaying),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    enable_logging(&args.log_dir);

    let (tx, mut rx) = mpsc::channel(1);

    info!("Start watching Jellyfin sessions");

    let jelly_client = jellyfin::Client::new(args.jellyfin_url, args.jellyfin_api_key);
    tokio::spawn(jellyfin::watch(
        Duration::from_secs(args.interval),
        jelly_client,
        tx,
    ));

    let sonarr_client = sonarr::Client::new(args.sonarr_url, args.sonarr_api_key);

    let mut seen = Seen::default();

    while let Some(msg) = rx.recv().await {
        match msg {
            Message::NowPlaying(np) => {
                if let Err(e) = search_next(np, &sonarr_client, &mut seen).await {
                    error!(err = ?e, "Failed to process");
                }
            }
        };
    }

    Ok(())
}

fn enable_logging(log_dir: &Option<PathBuf>) {
    if let Some(log_dir) = log_dir {
        let file_appender = tracing_appender::rolling::daily(log_dir, "prefetcharr.log");
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_ansi(false)
            .with_writer(file_appender)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
}
