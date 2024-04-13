#![warn(clippy::pedantic)]

use std::{
    future::Future,
    io::{stderr, IsTerminal},
    path::PathBuf,
    pin::Pin,
    time::Duration,
};

use clap::{arg, command, Parser, ValueEnum};
use tokio::sync::mpsc;
use tracing::{info, level_filters::LevelFilter, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::{
    media_server::{plex, MediaServer as _},
    once::Seen,
};

mod media_server;
mod once;
mod process;
mod sonarr;

use media_server::embyfin;

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Media server type
    #[arg(long, default_value = "jellyfin")]
    media_server_type: MediaServer,
    /// Jellyfin/Emby/Plex baseurl
    #[arg(long, alias = "jellyfin-url", value_name = "URL")]
    media_server_url: String,
    /// Jellyfin/Emby API key or Plex server token
    #[arg(
        long,
        value_name = "API_KEY",
        required_unless_present = "jellyfin_api_key",
        env = "MEDIA_SERVER_API_KEY"
    )]
    media_server_api_key: Option<String>,
    #[arg(long, hide = true, env = "JELLYFIN_API_KEY")]
    jellyfin_api_key: Option<String>,
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
    /// The last <NUM> episodes trigger a search
    #[arg(long, value_name = "NUM", default_value_t = 2)]
    remaining_episodes: u8,
}

#[derive(Clone, Debug, ValueEnum)]
enum MediaServer {
    Jellyfin,
    Emby,
    Plex,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Message {
    NowPlaying(media_server::NowPlaying),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    enable_logging(&args.log_dir);

    info!("{NAME} {VERSION}");
    warn_deprecated(&args);

    let (tx, rx) = mpsc::channel(1);

    // backward compat
    let media_server_api_key = args
        .media_server_api_key
        .or(args.jellyfin_api_key)
        .expect("using value enforced via clap");

    let watcher: Pin<Box<dyn Future<Output = ()> + Send>> = match args.media_server_type {
        MediaServer::Jellyfin => {
            info!("Start watching Jellyfin sessions");
            let client = embyfin::Client::new(
                &args.media_server_url,
                &media_server_api_key,
                embyfin::Fork::Jellyfin,
            )?;
            Box::pin(client.watch(Duration::from_secs(args.interval), tx))
        }
        MediaServer::Emby => {
            info!("Start watching Emby sessions");
            let client = embyfin::Client::new(
                &args.media_server_url,
                &media_server_api_key,
                embyfin::Fork::Emby,
            )?;
            Box::pin(client.watch(Duration::from_secs(args.interval), tx))
        }
        MediaServer::Plex => {
            info!("Start watching Plex sessions");
            let client = plex::Client::new(&args.media_server_url, &media_server_api_key)?;
            Box::pin(client.watch(Duration::from_secs(args.interval), tx))
        }
    };

    let sonarr_client = sonarr::Client::new(args.sonarr_url, args.sonarr_api_key);
    let seen = Seen::default();
    let mut actor = process::Actor::new(rx, sonarr_client, seen, args.remaining_episodes);

    tokio::join!(watcher, actor.process());

    Ok(())
}

fn enable_logging(log_dir: &Option<PathBuf>) {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(stderr().is_terminal())
        .with_writer(stderr)
        .finish();

    let rolling_layer = log_dir.as_ref().map(|log_dir| {
        let file_appender = tracing_appender::rolling::daily(log_dir, "prefetcharr.log");
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file_appender)
    });

    subscriber
        .with(rolling_layer)
        .try_init()
        .expect("setting the default subscriber");
}

fn warn_deprecated(args: &Args) {
    if std::env::args().any(|arg| arg == "--jellyfin-url") {
        warn!("`--jellyfin-url` is deprecated. Use `--media-server-url` instead.");
    }
    if args.jellyfin_api_key.is_some() {
        warn!("`JELLYFIN_API_KEY` is deprecated. Use `MEDIA_SERVER_API_KEY` instead.");
    }
}
