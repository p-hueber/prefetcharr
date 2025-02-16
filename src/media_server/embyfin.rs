use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use futures::{
    future::{BoxFuture, LocalBoxFuture},
    stream::BoxStream,
    FutureExt,
};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Url,
};
use rustls_platform_verifier::ConfigVerifierExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::MediaServer;

use super::{NowPlaying, ProvideNowPlaying};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Episode {
    series_id: String,
    season_id: String,
    index_number: i32,
    #[serde(flatten)]
    _other: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Season {
    index_number: i32,
    #[serde(flatten)]
    _other: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Series {
    name: String,
    provider_ids: HashMap<String, String>,
    #[serde(flatten)]
    _other: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SessionInfo {
    user_id: String,
    user_name: String,
    now_playing_item: Episode,
    #[serde(flatten)]
    other: serde_json::Value,
}

#[derive(Clone, Copy)]
pub enum Fork {
    Jellyfin,
    Emby,
}

impl TryFrom<MediaServer> for Fork {
    type Error = anyhow::Error;

    fn try_from(value: MediaServer) -> std::result::Result<Self, Self::Error> {
        match value {
            MediaServer::Jellyfin => Ok(Self::Jellyfin),
            MediaServer::Emby => Ok(Self::Emby),
            MediaServer::Plex => Err(anyhow!("media server is neither Emby nor Jellyfin")),
        }
    }
}

pub struct Client {
    base_url: Url,
    http: reqwest::Client,
    fork: Fork,
}

impl Client {
    pub fn new(base_url: &str, api_key: &str, fork: Fork) -> Result<Self> {
        let base_url = base_url.parse()?;

        let mut headers = HeaderMap::new();
        match fork {
            Fork::Jellyfin => {
                let value = format!("MediaBrowser Token=\"{api_key}\"");

                let mut value = HeaderValue::from_str(&value)?;
                value.set_sensitive(true);

                headers.insert("Authorization", value);
            }
            Fork::Emby => {
                let mut token = HeaderValue::from_str(api_key)?;
                token.set_sensitive(true);

                headers.insert("X-Emby-Token", token);
            }
        }

        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .use_preconfigured_tls(rustls::ClientConfig::with_platform_verifier())
            .build()?;

        Ok(Self {
            base_url,
            http,
            fork,
        })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .extend(path.split('/'));
        let response = self.http.get(url).send().await?.error_for_status()?;
        Ok(response.json::<T>().await?)
    }

    async fn item<T: DeserializeOwned>(&self, user_id: &str, item_id: &str) -> Result<T> {
        let path = format!("Users/{user_id}/Items/{item_id}");
        self.get(path.as_str()).await
    }
}

#[derive(Debug)]
struct Ids {
    user: String,
    series: String,
    season: String,
}

impl From<SessionInfo> for Ids {
    fn from(session: SessionInfo) -> Self {
        Ids::new(&session)
    }
}

impl Ids {
    fn new(session: &SessionInfo) -> Self {
        let user_id = session.user_id.clone();
        let np = &session.now_playing_item;
        let series_id = np.series_id.clone();
        let season_id = np.season_id.clone();

        Self {
            user: user_id,
            series: series_id,
            season: season_id,
        }
    }
}

impl super::ProvideNowPlaying for Client {
    type Session = SessionInfo;

    async fn sessions(&self) -> anyhow::Result<Vec<Self::Session>> {
        Ok(self
            .get::<Vec<Value>>("Sessions")
            .await?
            .iter()
            .cloned()
            .map(serde_json::from_value)
            .filter_map(Result::ok)
            .collect::<Vec<Self::Session>>())
    }

    async fn extract(&self, session: Self::Session) -> anyhow::Result<NowPlaying> {
        let episode_num = session.now_playing_item.index_number;
        let user_id = session.user_id.clone();
        let user_name = session.user_name.clone();
        let ids = Ids::from(session);

        let series: Series = self.item(&ids.user, &ids.series).await?;

        let season: Season = self.item(&ids.user, &ids.season).await?;
        let season_num = season.index_number;

        let tvdb_id = series.provider_ids.get("Tvdb");

        let series = match tvdb_id {
            Some(tvdb) => super::Series::Tvdb(tvdb.parse()?),
            None => super::Series::Title(series.name),
        };

        let now_playing = NowPlaying {
            series,
            episode: episode_num,
            season: season_num,
            user_id,
            user_name,
        };

        Ok(now_playing)
    }
}

impl super::Client for Client {
    fn now_playing(&self) -> BoxFuture<anyhow::Result<BoxStream<NowPlaying>>> {
        ProvideNowPlaying::now_playing(self)
    }

    fn probe(&self) -> LocalBoxFuture<anyhow::Result<()>> {
        async move {
            let name = match self.fork {
                Fork::Jellyfin => "Jellyfin",
                Fork::Emby => "Emby",
            };
            self.get::<Value>("System/Endpoint")
                .await
                .with_context(|| format!("Probing {name} failed"))?;
            Ok(())
        }
        .boxed_local()
    }
}

#[cfg(test)]
mod test {
    use std::time::{Duration, Instant};

    use futures::StreamExt;

    use crate::media_server::{embyfin, Client, NowPlaying, Series};

    fn episode() -> serde_json::Value {
        serde_json::json!(
            [{
                "UserId": "08ba1929-681e-4b24-929b-9245852f65c0",
                "UserName": "user",
                "NowPlayingItem": {
                    "SeriesId": "a",
                    "SeasonId": "b",
                    "IndexNumber": 5
                }
            }]
        )
    }
    fn series() -> serde_json::Value {
        serde_json::json!({
            "Name": "Test Show",
            "ProviderIds": { "Tvdb": "1234" }
        })
    }

    #[tokio::test]
    async fn single_session() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(episode());
            })
            .await;

        let season_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/b");
                then.json_body(serde_json::json!({"IndexNumber": 3}));
            })
            .await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/a");
                then.json_body(series());
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Tvdb(1234),
            episode: 5,
            season: 3,
            user_id: "08ba1929-681e-4b24-929b-9245852f65c0".to_string(),
            user_name: "user".to_string(),
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;
        season_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn skip_invalid_sessions() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(serde_json::json!(
                    [{ "invalid": "session" },
                     {
                        "UserId": "08ba1929-681e-4b24-929b-9245852f65c0",
                        "UserName": "user",
                        "NowPlayingItem": {
                            "SeriesId": "invalid",
                            "SeasonId": "invalid",
                            "IndexNumber": 5
                        }
                     },
                     {
                        "UserId": "08ba1929-681e-4b24-929b-9245852f65c0",
                        "UserName": "user",
                        "NowPlayingItem": {
                            "SeriesId": "a",
                            "SeasonId": "b",
                            "IndexNumber": 5
                        }
                    }]
                ));
            })
            .await;

        let season_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/b");
                then.json_body(serde_json::json!({"IndexNumber": 3}));
            })
            .await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/a");
                then.json_body(series());
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Tvdb(1234),
            episode: 5,
            season: 3,
            user_id: "08ba1929-681e-4b24-929b-9245852f65c0".to_string(),
            user_name: "user".to_string(),
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;
        season_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn name_fallback_emby() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(episode());
            })
            .await;

        let season_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/b");
                then.json_body(serde_json::json!({"IndexNumber": 3}));
            })
            .await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/a");
                then.json_body(serde_json::json!({
                    "Name": "Test Show",
                    "ProviderIds": { }
                }));
            })
            .await;

        let client =
            embyfin::Client::new(&server.url("/pathprefix"), "secret", embyfin::Fork::Emby)?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Title("Test Show".to_string()),
            episode: 5,
            season: 3,
            user_id: "08ba1929-681e-4b24-929b-9245852f65c0".to_string(),
            user_name: "user".to_string(),
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;
        season_mock.assert_async().await;

        Ok(())
    }

    #[test]
    fn bad_url() {
        assert!(embyfin::Client::new("/notanurl", "secret", embyfin::Fork::Jellyfin,).is_err());
    }

    #[tokio::test]
    async fn interval() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let _sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(episode());
            })
            .await;

        let _season_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/b");
                then.json_body(serde_json::json!({"IndexNumber": 3}));
            })
            .await;

        let _series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/a");
                then.json_body(series());
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let mut np_updates = client.now_playing_updates(Duration::from_millis(100));

        let _ = np_updates.next().await;
        let start = Instant::now();
        let _ = np_updates.next().await;
        assert!(Instant::now().duration_since(start) >= Duration::from_millis(100));

        Ok(())
    }

    #[tokio::test]
    async fn jellyfin_auth() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions")
                    .header("Authorization", "MediaBrowser Token=\"secret\"");
                then.json_body(episode());
            })
            .await;

        let _season_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/b");
                then.json_body(serde_json::json!({"IndexNumber": 3}));
            })
            .await;

        let _series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/a");
                then.json_body(series());
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));

        let _ = np_updates.next().await;
        sessions_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn emby_auth() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions")
                    .header("X-Emby-Token", "secret");
                then.json_body(episode());
            })
            .await;

        let _season_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/b");
                then.json_body(serde_json::json!({"IndexNumber": 3}));
            })
            .await;

        let _series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Users/08ba1929-681e-4b24-929b-9245852f65c0/Items/a");
                then.json_body(series());
            })
            .await;

        let client =
            embyfin::Client::new(&server.url("/pathprefix"), "secret", embyfin::Fork::Emby)?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));

        let _ = np_updates.next().await;
        sessions_mock.assert_async().await;

        Ok(())
    }
}
