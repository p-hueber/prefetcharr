use std::time::Duration;

use clap::{arg, command, Parser};
use tokio::sync::mpsc;

mod jellyfin;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Jellyfin baseurl
    #[arg(long, value_name = "URL")]
    jellyfin_url: String,
    /// Jellyfin API key
    #[arg(long, value_name = "API_KEY", env = "JELLYFIN_API_KEY")]
    jellyfin_api_key: String,
}

pub enum Message {
    NowPlaying(jellyfin::NowPlaying),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let (tx, mut rx) = mpsc::channel(1);

    let client = jellyfin::Client::new(args.jellyfin_url, args.jellyfin_api_key);
    tokio::spawn(jellyfin::watch(Duration::from_secs(300), client, tx));

    while let Some(msg) = rx.recv().await {
        match msg {
            Message::NowPlaying(now_playing) => {
                println!("sessions: {now_playing:?}");
            }
        }
    }

    Ok(())
}
