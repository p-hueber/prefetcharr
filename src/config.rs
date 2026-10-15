use std::path::PathBuf;

use serde::Deserialize;

use crate::LegacyArgs;

#[derive(Deserialize)]
pub struct MediaServer {
    /// Media server type
    pub r#type: crate::MediaServer,
    /// Jellyfin/Emby/Plex/Tautulli baseurl
    pub url: String,
    /// Jellyfin/Emby/Tautulli API key or Plex server token
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
    /// Exclude series by tag
    pub exclude_tag: Option<String>,
    /// Tag to apply to a series when episodes are prefetched for it
    pub apply_tag: Option<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefetchNum {
    Count(usize),
    Percent(u8),
}

impl PrefetchNum {
    pub fn resolve(self, season_len: usize) -> usize {
        match self {
            Self::Count(n) => n,
            Self::Percent(p) => (season_len * usize::from(p)).div_ceil(100),
        }
    }
}

impl<'de> Deserialize<'de> for PrefetchNum {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Count(usize),
            Text(String),
        }

        match Raw::deserialize(deserializer)? {
            Raw::Count(n) => Ok(Self::Count(n)),
            Raw::Text(s) => {
                let s = s.trim();
                let p = s
                    .strip_suffix('%')
                    .map(str::trim)
                    .and_then(|p| p.parse::<u8>().ok())
                    .filter(|p| *p <= 100)
                    .ok_or_else(|| {
                        serde::de::Error::custom(format!(
                            "invalid prefetch_num {s:?}: expected a number or a percentage like \"50%\""
                        ))
                    })?;
                Ok(Self::Percent(p))
            }
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
    pub prefetch_num: PrefetchNum,
    /// Always request full seasons to prefer season packs
    pub request_seasons: bool,
    /// Number of retries for the initial connection probing
    pub connection_retries: usize,
    /// Append upcoming episodes to the active player queue
    #[serde(default)]
    pub append_to_queue: bool,
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
            exclude_tag: None,
            apply_tag: None,
        };
        Config {
            media_server,
            sonarr,
            interval,
            log_dir,
            log_level: None,
            prefetch_num: PrefetchNum::Count(remaining_episodes.into()),
            request_seasons: true,
            connection_retries,
            append_to_queue: false,
            legacy: true,
        }
    }
}

#[cfg(test)]
mod test {
    use super::PrefetchNum;

    #[derive(serde::Deserialize)]
    struct Wrap {
        prefetch_num: PrefetchNum,
    }

    fn parse(v: &str) -> Result<PrefetchNum, toml::de::Error> {
        toml::from_str::<Wrap>(&format!("prefetch_num = {v}")).map(|w| w.prefetch_num)
    }

    #[test]
    fn prefetch_num_parse() {
        assert_eq!(parse("2").unwrap(), PrefetchNum::Count(2));
        assert_eq!(parse("\"50%\"").unwrap(), PrefetchNum::Percent(50));
        assert_eq!(parse("\" 75 % \"").unwrap(), PrefetchNum::Percent(75));
        assert!(parse("\"101%\"").is_err());
        assert!(parse("\"50\"").is_err());
        assert!(parse("\"abc%\"").is_err());
    }

    #[test]
    fn prefetch_num_resolve() {
        assert_eq!(PrefetchNum::Count(3).resolve(10), 3);
        assert_eq!(PrefetchNum::Percent(25).resolve(8), 2);
        assert_eq!(PrefetchNum::Percent(50).resolve(10), 5);
        // rounded up
        assert_eq!(PrefetchNum::Percent(50).resolve(7), 4);
        assert_eq!(PrefetchNum::Percent(10).resolve(8), 1);
        assert_eq!(PrefetchNum::Percent(100).resolve(10), 10);
        assert_eq!(PrefetchNum::Percent(0).resolve(10), 0);
    }
}
