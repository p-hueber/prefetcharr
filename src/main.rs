#![warn(clippy::pedantic)]

use std::{path::PathBuf, time::Duration};

use anyhow::anyhow;
use clap::{arg, command, Parser};
use jellyfin::NowPlaying;
use tokio::sync::mpsc;
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

mod jellyfin;
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

    while let Some(msg) = rx.recv().await {
        match msg {
            Message::NowPlaying(np) => {
                if let Err(e) = process(np, &sonarr_client).await {
                    error!(err = ?e, "Failed to process");
                }
            }
        };
    }

    Ok(())
}

async fn process(np: NowPlaying, sonarr_client: &sonarr::Client) -> anyhow::Result<()> {
    let series = sonarr_client.series().await?;
    let mut series = series
        .into_iter()
        .find(|s| s.tvdb_id == np.tvdb)
        .ok_or_else(|| anyhow!("series not found in Sonarr"))?;

    info!(title = series.title.clone().unwrap_or_else(|| "?".to_string()), now_playing = ?np);

    let season = series
        .season(np.season)
        .ok_or_else(|| anyhow!("season not known to Sonarr"))?;

    // Fetch from the second-to-last episode so the next season is available when the
    // last episode plays. This should allow Jellyfin to display "next episode" in time.
    if np.episode < season.last_episode() - 1 {
        debug!(now_playing = ?np, season = ?season, "ignoring early episode");
        return Ok(());
    }

    let next_season = series
        .season_mut(np.season + 1)
        .ok_or_else(|| anyhow!("season not known to Sonarr"))?;
    if next_season.monitored {
        debug!(next_season = ?next_season, "season is monitored already");
        return Ok(());
    }

    let next_season_num = next_season.season_number;
    info!(num = next_season_num, "Searching next season");

    sonarr_client
        .search_season(&series, next_season_num)
        .await?;

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
