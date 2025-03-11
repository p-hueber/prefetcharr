#![warn(clippy::pedantic)]

use std::{
    fmt::Display,
    io::{IsTerminal, stderr},
    path::PathBuf,
    time::Duration,
};

use anyhow::Context as _;
use clap::{Parser, ValueEnum, arg, command};
use futures::{StreamExt as _, TryStreamExt};
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;
use tracing::{error, info, level_filters::LevelFilter, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{media_server::plex, util::once::Seen};

mod filter;
mod media_server;
mod process;
mod sonarr;
mod util;

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
    /// User IDs or names to monitor episodes for (default: empty/all users)
    ///
    /// Each entry here is checked against the user's ID and name
    #[arg(long, value_name = "USER", value_delimiter = ',', num_args = 0..)]
    users: Vec<String>,
    /// Number of retries for the initial connection probing
    #[arg(long, value_name = "NUM", default_value_t = 0)]
    connection_retries: usize,
    /// Library names to monitor episodes for. (default: empty/all libraries)
    #[arg(long, value_name = "LIBRARY", value_delimiter = ',', num_args = 0..)]
    libraries: Vec<String>,
}

#[derive(Clone, Debug, ValueEnum)]
enum MediaServer {
    Jellyfin,
    Emby,
    Plex,
}

impl Display for MediaServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            MediaServer::Jellyfin => "Jellyfin",
            MediaServer::Emby => "Emby",
            MediaServer::Plex => "Plex",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Message {
    NowPlaying(media_server::NowPlaying),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    enable_logging(args.log_dir.as_ref());

    info!("{NAME} {VERSION}");
    warn_deprecated(&args);

    if let Err(e) = run(args).await {
        error!("{e:#}");
        info!("{NAME} exits due to an error");
        return Err(e.into());
    }

    Ok(())
}

async fn run(args: Args) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel(1);

    // backward compat
    let media_server_api_key = args
        .media_server_api_key
        .or(args.jellyfin_api_key)
        .expect("using value enforced via clap");

    let sonarr_client = sonarr::Client::new(&args.sonarr_url, &args.sonarr_api_key)
        .context("Invalid connection parameters for Sonarr")?;
    util::retry(args.connection_retries, async || {
        sonarr_client.probe().await.context("Probing Sonarr failed")
    })
    .await?;

    info!("Start watching {} sessions", args.media_server_type);
    let interval = Duration::from_secs(args.interval);
    let client: Box<dyn media_server::Client> = match args.media_server_type {
        MediaServer::Emby | MediaServer::Jellyfin => {
            let client = embyfin::Client::new(
                &args.media_server_url,
                &media_server_api_key,
                args.media_server_type.try_into()?,
            )
            .context("Invalid connection parameters")?;
            Box::new(client)
        }
        MediaServer::Plex => {
            let client = plex::Client::new(&args.media_server_url, &media_server_api_key)
                .context("Invalid connection parameters")?;
            Box::new(client)
        }
    };

    client.probe_with_retry(args.connection_retries).await?;

    let sink = PollSender::new(tx);
    let np_updates = client
        .now_playing_updates(interval)
        .inspect_err(|err| error!("Cannot fetch sessions from media server: {err}"))
        .filter_map(async |res| res.ok()) // remove errors
        .filter(filter::users(args.users.as_slice()))
        .filter(filter::libraries(args.libraries.as_slice()))
        .map(Message::NowPlaying)
        .map(Ok) // align with the error type of `PollSender`
        .forward(sink);

    let seen = Seen::default();
    let mut actor = process::Actor::new(rx, sonarr_client, seen, args.remaining_episodes);

    let _ = tokio::join!(np_updates, actor.process());

    Ok(())
}

fn enable_logging(log_dir: Option<&PathBuf>) {
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
