#![warn(clippy::pedantic)]

use std::{
    fmt::Display,
    fs::read_to_string,
    io::{IsTerminal, stderr},
    path::PathBuf,
    time::Duration,
};

use anyhow::Context;
use clap::{Parser, ValueEnum, arg, command};
use config::Config;
use futures::{StreamExt as _, TryStreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{media_server::plex, util::once::Seen};

mod config;
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
    /// Path to config file
    #[arg(long)]
    config: PathBuf,
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct LegacyArgs {
    /// Media server type
    #[arg(long, default_value = "jellyfin")]
    media_server_type: MediaServer,
    /// Jellyfin/Emby/Plex/Tautulli baseurl
    #[arg(long, value_name = "URL")]
    media_server_url: String,
    /// Jellyfin/Emby/Tautulli API key or Plex server token
    #[arg(long, value_name = "API_KEY", env = "MEDIA_SERVER_API_KEY")]
    media_server_api_key: String,
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

#[derive(Clone, Debug, Deserialize, ValueEnum)]
enum MediaServer {
    Jellyfin,
    Emby,
    Plex,
    Tautulli,
}

impl Display for MediaServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            MediaServer::Jellyfin => "Jellyfin",
            MediaServer::Emby => "Emby",
            MediaServer::Plex => "Plex",
            MediaServer::Tautulli => "Tautulli",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Message {
    NowPlaying(media_server::NowPlaying),
}

fn config() -> anyhow::Result<Config> {
    if let Ok(args) = LegacyArgs::try_parse() {
        Ok(Config::from(args))
    } else {
        let args = Args::parse();
        let toml = read_to_string(args.config.as_path())
            .with_context(|| format!("reading config from {}", args.config.to_string_lossy()))?;
        let config = toml::from_str(&toml).context("parsing TOML config")?;
        Ok(config)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = config()?;

    enable_logging(config.log_dir.as_ref(), config.log_level);

    info!("{NAME} {VERSION}");

    if config.legacy {
        warn!(
            "Legacy configuration method detected. \
            Migrate to the new TOML config to avoid future problems."
        );
    }

    if let Err(e) = run(config).await {
        error!("{e:#}");
        info!("{NAME} exits due to an error");
        return Err(e.into());
    }

    Ok(())
}

async fn run(config: Config) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel(1);

    let sonarr_client = sonarr::Client::new(&config.sonarr.url, &config.sonarr.api_key)
        .context("Invalid connection parameters for Sonarr")?;
    util::retry(config.connection_retries, async || {
        sonarr_client.probe().await.context("Probing Sonarr failed")
    })
    .await?;

    info!("Start watching {} sessions", config.media_server.r#type);
    let interval = Duration::from_secs(config.interval);
    let client: Box<dyn media_server::Client> = match config.media_server.r#type {
        MediaServer::Emby | MediaServer::Jellyfin => {
            let client = embyfin::Client::new(
                &config.media_server.url,
                &config.media_server.api_key,
                config.media_server.r#type.try_into()?,
            )
            .context("Invalid connection parameters")?;
            Box::new(client)
        }
        MediaServer::Plex => {
            let client = plex::Client::new(&config.media_server.url, &config.media_server.api_key)
                .context("Invalid connection parameters")?;
            Box::new(client)
        }
        MediaServer::Tautulli => {
            let client = media_server::tautulli::Client::new(
                &config.media_server.url,
                &config.media_server.api_key,
            )
            .context("Invalid connection parameters")?;
            Box::new(client)
        }
    };

    client.probe_with_retry(config.connection_retries).await?;

    let sink = PollSender::new(tx);
    let np_updates = client
        .now_playing_updates(interval)
        .inspect_err(|err| error!("Cannot fetch sessions from media server: {err}"))
        .filter_map(async |res| res.ok()) // remove errors
        .filter(filter::users(config.media_server.users.as_slice()))
        .filter(filter::libraries(config.media_server.libraries.as_slice()))
        .map(Message::NowPlaying)
        .map(Ok) // align with the error type of `PollSender`
        .forward(sink);

    let seen = Seen::default();
    let mut actor = process::Actor::new(
        rx,
        sonarr_client,
        seen,
        config.prefetch_num,
        config.request_seasons,
    );

    let _ = tokio::join!(np_updates, actor.process());

    Ok(())
}

fn enable_logging(log_dir: Option<&PathBuf>, level: Option<config::LogLevel>) {
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(stderr().is_terminal())
        .with_writer(stderr)
        .finish();

    let filter = if let Some(level) = level {
        tracing_subscriber::filter::Targets::new()
            .with_target(env!("CARGO_PKG_NAME"), tracing::Level::from(level))
            .boxed()
    } else {
        EnvFilter::builder().from_env_lossy().boxed()
    };

    let rolling_layer = log_dir.as_ref().map(|log_dir| {
        let file_appender = tracing_appender::rolling::daily(log_dir, "prefetcharr.log");
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file_appender)
    });

    subscriber
        .with(filter)
        .with(rolling_layer)
        .try_init()
        .expect("setting the default subscriber");
}
