use std::path::PathBuf;

use serde::Deserialize;

use crate::Args;

#[derive(Deserialize)]
pub struct MediaServer {
    /// Media server type
    pub r#type: crate::MediaServer,
    /// Jellyfin/Emby/Plex baseurl
    pub url: String,
    /// Jellyfin/Emby API key or Plex server token
    pub api_key: String,
    /// User IDs or names to monitor episodes for (default: empty/all users)
    pub users: Vec<String>,
    /// Library names to monitor episodes for. (default: empty/all libraries)
    pub libraries: Vec<String>,
}

#[derive(Deserialize)]
pub struct Sonarr {
    /// Sonarr baseurl
    pub url: String,
    /// Sonarr API key
    pub api_key: String,
}

#[derive(Deserialize)]
pub struct Config {
    pub media_server: MediaServer,
    pub sonarr: Sonarr,
    /// Polling interval
    pub interval: u64,
    /// Logging directory
    pub log_dir: Option<PathBuf>,
    /// The last <NUM> episodes trigger a search
    pub remaining_episodes: u8,
    /// Number of retries for the initial connection probing
    pub connection_retries: usize,
}

impl From<Args> for Config {
    fn from(
        Args {
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
        }: Args,
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
            remaining_episodes,
            connection_retries,
        }
    }
}
