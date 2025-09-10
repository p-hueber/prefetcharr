use std::path::PathBuf;

use serde::Deserialize;

use crate::LegacyArgs;

#[derive(Deserialize)]
pub struct MediaServer {
    /// Media server type
    pub r#type: crate::MediaServer,
    /// Jellyfin/Emby/Plex baseurl
    pub url: String,
    /// Jellyfin/Emby API key or Plex server token
    pub api_key: String,
    /// User IDs or names to monitor episodes for (default: empty/all users)
    #[serde(default)]
    pub users: Vec<String>,
    /// Library names to monitor episodes for. (default: empty/all libraries)
    #[serde(default)]
    pub libraries: Vec<String>,
}

#[derive(Deserialize)]
pub struct Sonarr {
    /// Sonarr baseurl
    pub url: String,
    /// Sonarr API key
    pub api_key: String,
}

#[derive(Clone, Copy, Deserialize)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<LogLevel> for tracing::Level {
    fn from(value: LogLevel) -> Self {
        match value {
            LogLevel::Trace => tracing::Level::TRACE,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Error => tracing::Level::ERROR,
        }
    }
}

#[derive(Deserialize)]
pub struct Config {
    pub media_server: MediaServer,
    pub sonarr: Sonarr,
    /// Polling interval
    pub interval: u64,
    /// Logging directory
    pub log_dir: Option<PathBuf>,
    /// Log level
    pub log_level: Option<LogLevel>,
    /// Number of episodes to make available in advance
    pub prefetch_num: usize,
    /// Always request full seasons to prefer season packs
    pub request_seasons: bool,
    /// Number of retries for the initial connection probing
    pub connection_retries: usize,
    #[serde(default)]
    pub legacy: bool,
}

impl From<LegacyArgs> for Config {
    fn from(
        LegacyArgs {
            media_server_type,
            media_server_url,
            media_server_api_key,
            sonarr_url,
            sonarr_api_key,
            interval,
            log_dir,
            remaining_episodes,
            users,
            connection_retries,
            libraries,
        }: LegacyArgs,
    ) -> Self {
        let media_server = MediaServer {
            r#type: media_server_type,
            url: media_server_url,
            api_key: media_server_api_key,
            users,
            libraries,
        };
        let sonarr = Sonarr {
            url: sonarr_url,
            api_key: sonarr_api_key,
        };
        Config {
            media_server,
            sonarr,
            interval,
            log_dir,
            log_level: None,
            prefetch_num: remaining_episodes.into(),
            request_seasons: true,
            connection_retries,
            legacy: true,
        }
    }
}
