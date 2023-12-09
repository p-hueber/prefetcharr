use clap::{arg, command, Parser};

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let client = jellyfin::Client::new(args.jellyfin_url, args.jellyfin_api_key);

    let sessions = client.sessions().await?;

    let now_playing: Vec<_> = sessions
        .into_iter()
        .map(|s| (s.user_name, s.now_playing_item))
        .collect();

    println!("sessions: {now_playing:?}");

    Ok(())
}
