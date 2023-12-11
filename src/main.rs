use std::time::Duration;

use clap::{arg, command, Parser};
use tokio::sync::mpsc;

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
}

pub enum Message {
    NowPlaying(jellyfin::NowPlaying),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let (tx, mut rx) = mpsc::channel(1);

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
                let series = sonarr_client.series().await.unwrap();
                let mut series = series.into_iter().find(|s| s.tvdb_id == np.tvdb).unwrap();

                println!(
                    "NowPlaying: {} {:?}",
                    series.title.clone().unwrap_or_else(|| "?".to_string()),
                    np
                );

                let season = series.season(np.season).unwrap();

                // Fetch from the second-to-last episode so the next season is available when the
                // last episode plays. This should allow Jellyfin to display "next episode" in time.
                if np.episode < season.last_episode() - 1 {
                    continue;
                }

                let next_season = series.season_mut(np.season + 1).unwrap();
                if next_season.monitored {
                    continue;
                }

                println!("Searching next season");

                let next_season_num = next_season.season_number;

                sonarr_client
                    .search_season(&series, next_season_num)
                    .await
                    .unwrap();
            }
        }
    }

    Ok(())
}
