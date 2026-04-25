use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow};
use futures::{
    FutureExt,
    future::{BoxFuture, LocalBoxFuture},
    stream::BoxStream,
};
use reqwest::{
    Url,
    header::{HeaderMap, HeaderValue},
};
use rustls_platform_verifier::ConfigVerifierExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tracing::{debug, info};

use crate::MediaServer;

use super::{NowPlaying, ProvideNowPlaying};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Episode {
    series_id: String,
    season_id: String,
    index_number: i32,
    path: String,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct VirtualFolderInfo {
    name: String,
    locations: Vec<String>,
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
            MediaServer::Plex | MediaServer::Tautulli => {
                Err(anyhow!("media server is neither Emby nor Jellyfin"))
            }
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

    async fn virtual_folders(&self) -> Result<Vec<VirtualFolderInfo>> {
        self.get("Library/VirtualFolders").await
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
        let ids = Ids::new(&session);
        let episode_num = session.now_playing_item.index_number;
        let user = super::User {
            id: session.user_id,
            name: session.user_name,
        };
        let series: Series = self.item(&ids.user, &ids.series).await?;

        let season: Season = self.item(&ids.user, &ids.season).await?;
        let season_num = season.index_number;

        let tvdb_id = series.provider_ids.get("Tvdb");

        let series = match tvdb_id {
            Some(tvdb) => super::Series::Tvdb(tvdb.parse()?),
            None => super::Series::Title(series.name),
        };

        let item_path = session.now_playing_item.path.clone();
        let library = {
            let folders = self.virtual_folders().await?;
            let folder = folders.into_iter().find(|f| {
                f.locations
                    .iter()
                    .any(|l| item_path.starts_with(l.as_str()))
            });
            folder.map(|f| f.name)
        };

        let session_id = session
            .other
            .get("Id")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let now_playing = NowPlaying {
            series,
            episode: episode_num,
            season: season_num,
            user,
            library,
            session_id,
        };

        Ok(now_playing)
    }
}

impl super::Client for Client {
    fn now_playing(&self) -> BoxFuture<'_, anyhow::Result<BoxStream<'_, NowPlaying>>> {
        ProvideNowPlaying::now_playing(self)
    }

    fn probe(&self) -> LocalBoxFuture<'_, anyhow::Result<()>> {
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

impl super::Queue for Client {
    fn append(
        &self,
        np: &NowPlaying,
        episodes: &[(i32, i32)],
    ) -> BoxFuture<'_, Result<HashSet<(i32, i32)>>> {
        let np = np.clone();
        let episodes = episodes.to_vec();
        async move {
            let session_id = np
                .session_id
                .as_deref()
                .ok_or_else(|| anyhow!("session_id missing"))?;

            let raw_sessions: Vec<Value> = self.get("Sessions").await?;
            let session = raw_sessions
                .into_iter()
                .find(|s| s.get("Id").and_then(Value::as_str) == Some(session_id))
                .ok_or_else(|| anyhow!("session {session_id} no longer exists"))?;

            let series_id = session
                .get("NowPlayingItem")
                .and_then(|i| i.get("SeriesId"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("session has no SeriesId"))?
                .to_string();
            let user_id = session
                .get("UserId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("session has no UserId"))?
                .to_string();

            let existing: HashSet<String> = session
                .get("NowPlayingQueue")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|i| i.get("Id").and_then(Value::as_str).map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let episode_map = self.list_series_episodes(&series_id, &user_id).await?;

            let mut success: HashSet<(i32, i32)> = HashSet::new();
            let mut to_post: Vec<((i32, i32), String)> = Vec::new();
            // Episodes arrive in playback order; stop at the first gap
            // (episode not yet in the library) so we never enqueue an episode
            // whose predecessor is still missing.
            for &(s, e) in &episodes {
                let Some(item_id) = episode_map.get(&(s, e)) else {
                    debug!(season = s, episode = e, "episode not yet in library; stop");
                    break;
                };
                if existing.contains(item_id) {
                    debug!(season = s, episode = e, "episode already in queue");
                    success.insert((s, e));
                } else {
                    to_post.push(((s, e), item_id.clone()));
                }
            }

            if !to_post.is_empty() {
                let item_ids = to_post
                    .iter()
                    .map(|(_, id)| id.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                let mut url = self.base_url.clone();
                url.path_segments_mut()
                    .map_err(|()| anyhow!("url is relative"))?
                    .extend(["Sessions", session_id, "Playing"]);
                url.query_pairs_mut()
                    .append_pair("playCommand", "PlayLast")
                    .append_pair("itemIds", &item_ids);
                self.http
                    .post(url)
                    .send()
                    .await?
                    .error_for_status()
                    .context("POST /Sessions/{}/Playing failed")?;
                let labels: Vec<String> = to_post
                    .iter()
                    .map(|((s, e), _)| format!("s{s:02}e{e:02}"))
                    .collect();
                info!(episodes = ?labels, "appended to play queue");
                for (pair, _) in to_post {
                    success.insert(pair);
                }
            }

            Ok(success)
        }
        .boxed()
    }
}

impl Client {
    async fn list_series_episodes(
        &self,
        series_id: &str,
        user_id: &str,
    ) -> Result<HashMap<(i32, i32), String>> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .extend(["Shows", series_id, "Episodes"]);
        url.query_pairs_mut()
            .append_pair("userId", user_id)
            .append_pair("fields", "ParentIndexNumber,IndexNumber");

        let response: Value = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let items = response
            .get("Items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut map = HashMap::new();
        for item in items {
            let season = item
                .get("ParentIndexNumber")
                .and_then(Value::as_i64)
                .and_then(|x| i32::try_from(x).ok());
            let episode = item
                .get("IndexNumber")
                .and_then(Value::as_i64)
                .and_then(|x| i32::try_from(x).ok());
            let id = item.get("Id").and_then(Value::as_str).map(String::from);
            if let (Some(s), Some(e), Some(id)) = (season, episode, id) {
                map.insert((s, e), id);
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod test {
    use std::time::{Duration, Instant};

    use futures::StreamExt;

    use std::collections::HashSet;

    use crate::media_server::{
        Client, NowPlaying, ProvideNowPlaying, Queue, Series, embyfin,
        test::{idle_pending, np_default},
    };

    fn episode() -> serde_json::Value {
        serde_json::json!(
            [{
                "UserId": "08ba1929-681e-4b24-929b-9245852f65c0",
                "UserName": "user",
                "NowPlayingItem": {
                    "SeriesId": "a",
                    "SeasonId": "b",
                    "IndexNumber": 5,
                    "Path": "/media/tv/a/b/c.mkv"
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

    fn virtual_folders() -> serde_json::Value {
        serde_json::json!(
        [
            {
                "Name": "Kids Movies",
                "Locations": [
                    "/media/KidsMovies"
                ],
                "CollectionType": "movies",
                "LibraryOptions": {},
                "ItemId": "7",
                "Id": "7",
                "Guid": "5ca6e6c767144c3284fb91a2f8e0523c",
                "PrimaryImageItemId": "7"
            },
            {
                "Name": "Kids TV Shows",
                "Locations": [
                    "/media/KidsTV"
                ],
                "CollectionType": "tvshows",
                "LibraryOptions": {},
                "ItemId": "9",
                "Id": "9",
                "Guid": "4a46715108f54dfc8f6418bad7e46718",
                "PrimaryImageItemId": "9"
            },
            {
                "Name": "Movies",
                "Locations": [
                    "/media/Movies"
                ],
                "CollectionType": "movies",
                "LibraryOptions": {},
                "ItemId": "3",
                "Id": "3",
                "Guid": "963293cee9894fb09580d46eddd61fe5",
                "PrimaryImageItemId": "3"
            },
            {
                "Name": "Playlists",
                "Locations": [
                    "/config/data/playlists",
                    "/config/data/userplaylists"
                ],
                "CollectionType": "playlists",
                "LibraryOptions": {},
                "ItemId": "36639",
                "Id": "36639",
                "Guid": "04c519cbf764481e9b66448210d2c055",
                "PrimaryImageItemId": "36639"
            },
            {
                "Name": "TV Shows",
                "Locations": [
                    "/media/tv"
                ],
                "CollectionType": "tvshows",
                "LibraryOptions": {},
                "ItemId": "5",
                "Id": "5",
                "Guid": "26ee32dcd90f4607807874f09ced0fed",
                "PrimaryImageItemId": "5"
            },
            {
                "Name": "Collections",
                "Locations": [],
                "CollectionType": "boxsets",
                "LibraryOptions": {},
                "ItemId": "80071",
                "Id": "80071",
                "Guid": "a09546b017694aaa9f87c298823f91c0",
                "PrimaryImageItemId": "80071"
            }
        ]
                )
    }

    // Extracts NowPlaying from a Jellyfin session with TVDB ID and library matching
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

        let folders_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Library/VirtualFolders");
                then.json_body(virtual_folders());
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100), idle_pending());
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            library: Some("TV Shows".to_string()),
            ..np_default()
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;
        season_mock.assert_async().await;
        folders_mock.assert_async().await;

        Ok(())
    }

    // Invalid sessions and failed item lookups are skipped, valid sessions still extracted
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
                            "IndexNumber": 5,
                            "Path": "/media/a/b/c.mkv"
                        }
                     },
                     {
                        "UserId": "08ba1929-681e-4b24-929b-9245852f65c0",
                        "UserName": "user",
                        "NowPlayingItem": {
                            "SeriesId": "a",
                            "SeasonId": "b",
                            "IndexNumber": 5,
                            "Path": "/media/a/b/c.mkv"
                        },
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

        let _folders_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Library/VirtualFolders");
                then.json_body(serde_json::json!([]));
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100), idle_pending());
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = np_default();

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;
        season_mock.assert_async().await;

        Ok(())
    }

    // Falls back to series name when no TVDB provider ID exists (Emby fork)
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

        let _folders_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Library/VirtualFolders");
                then.json_body(serde_json::json!([]));
            })
            .await;

        let client =
            embyfin::Client::new(&server.url("/pathprefix"), "secret", embyfin::Fork::Emby)?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100), idle_pending());
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Title("Test Show".to_string()),
            ..np_default()
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;
        season_mock.assert_async().await;

        Ok(())
    }

    // Rejects invalid URLs at client construction time
    #[test]
    fn bad_url() {
        assert!(embyfin::Client::new("/notanurl", "secret", embyfin::Fork::Jellyfin,).is_err());
    }

    // Polling respects the configured interval between session updates
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

        let _folders_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Library/VirtualFolders");
                then.json_body(serde_json::json!([]));
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let mut np_updates = client.now_playing_updates(Duration::from_millis(100), idle_pending());

        let start = Instant::now();
        let _ = np_updates.next().await;
        let tolerance = Instant::now().duration_since(start) * 2;

        let start = Instant::now();
        let _ = np_updates.next().await;
        assert!(
            Instant::now().duration_since(start)
                >= Duration::from_millis(100).checked_sub(tolerance).unwrap()
        );

        Ok(())
    }

    // Jellyfin sends the API key via MediaBrowser Token authorization header
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

        let _folders_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Library/VirtualFolders");
                then.json_body(serde_json::json!([]));
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100), idle_pending());

        let _ = np_updates.next().await;
        sessions_mock.assert_async().await;

        Ok(())
    }

    // Emby sends the API key via X-Emby-Token header
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

        let _folders_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Library/VirtualFolders");
                then.json_body(serde_json::json!([]));
            })
            .await;

        let client =
            embyfin::Client::new(&server.url("/pathprefix"), "secret", embyfin::Fork::Emby)?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100), idle_pending());

        let _ = np_updates.next().await;
        sessions_mock.assert_async().await;

        Ok(())
    }

    fn sessions_with_queue(queue: &serde_json::Value) -> serde_json::Value {
        serde_json::json!([{
            "Id": "session-1",
            "UserId": "user-1",
            "UserName": "user",
            "NowPlayingItem": {
                "SeriesId": "series-1",
                "SeasonId": "b",
                "IndexNumber": 1,
                "Path": "/x"
            },
            "NowPlayingQueue": queue
        }])
    }

    fn show_episodes() -> serde_json::Value {
        serde_json::json!({
            "Items": [
                {"Id": "ep-1-1", "ParentIndexNumber": 1, "IndexNumber": 1},
                {"Id": "ep-1-2", "ParentIndexNumber": 1, "IndexNumber": 2},
                {"Id": "ep-1-3", "ParentIndexNumber": 1, "IndexNumber": 3}
            ]
        })
    }

    // Queue::append issues a single PlayLast POST with comma-joined item IDs
    #[tokio::test]
    async fn queue_append_success() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(sessions_with_queue(&serde_json::json!([
                    {"Id": "ep-1-1", "PlaylistItemId": "pi-1"}
                ])));
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Shows/series-1/Episodes");
                then.json_body(show_episodes());
            })
            .await;

        let post_mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/pathprefix/Sessions/session-1/Playing")
                    .query_param("playCommand", "PlayLast")
                    .query_param("itemIds", "ep-1-2,ep-1-3");
                then.status(204);
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;
        let np = NowPlaying {
            series: Series::Tvdb(1234),
            session_id: Some("session-1".into()),
            ..np_default()
        };
        let result = Queue::append(&client, &np, &[(1, 2), (1, 3)]).await?;

        let expected: HashSet<(i32, i32)> = [(1, 2), (1, 3)].into_iter().collect();
        assert_eq!(result, expected);

        sessions_mock.assert_async().await;
        episodes_mock.assert_async().await;
        post_mock.assert_async().await;
        Ok(())
    }

    // Episodes already in the player's NowPlayingQueue are reported as success
    // without issuing the POST
    #[tokio::test]
    async fn queue_append_dedup_existing() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let _sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(sessions_with_queue(&serde_json::json!([
                    {"Id": "ep-1-1"},
                    {"Id": "ep-1-2"}
                ])));
            })
            .await;

        let _episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Shows/series-1/Episodes");
                then.json_body(show_episodes());
            })
            .await;

        let post_mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/pathprefix/Sessions/session-1/Playing")
                    .query_param("itemIds", "ep-1-3");
                then.status(204);
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;
        let np = NowPlaying {
            session_id: Some("session-1".into()),
            ..np_default()
        };
        let result = Queue::append(&client, &np, &[(1, 2), (1, 3)]).await?;

        let expected: HashSet<(i32, i32)> = [(1, 2), (1, 3)].into_iter().collect();
        assert_eq!(result, expected);

        post_mock.assert_async().await;
        Ok(())
    }

    // A trailing episode missing from the library stops the append; earlier
    // pairs in playback order still go through.
    #[tokio::test]
    async fn queue_append_episode_missing() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let _sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(sessions_with_queue(&serde_json::json!([])));
            })
            .await;

        let _episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Shows/series-1/Episodes");
                then.json_body(serde_json::json!({
                    "Items": [
                        {"Id": "ep-1-2", "ParentIndexNumber": 1, "IndexNumber": 2}
                    ]
                }));
            })
            .await;

        let post_mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/pathprefix/Sessions/session-1/Playing")
                    .query_param("itemIds", "ep-1-2");
                then.status(204);
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;
        let np = NowPlaying {
            session_id: Some("session-1".into()),
            ..np_default()
        };
        let result = Queue::append(&client, &np, &[(1, 2), (1, 3)]).await?;

        let expected: HashSet<(i32, i32)> = [(1, 2)].into_iter().collect();
        assert_eq!(result, expected);

        post_mock.assert_async().await;
        Ok(())
    }

    // If the first episode in playback order is missing from the library, no
    // append is issued at all — we never want to skip ahead.
    #[tokio::test]
    async fn queue_append_stops_at_leading_gap() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let _sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(sessions_with_queue(&serde_json::json!([])));
            })
            .await;

        let _episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Shows/series-1/Episodes");
                then.json_body(serde_json::json!({
                    "Items": [
                        {"Id": "ep-1-3", "ParentIndexNumber": 1, "IndexNumber": 3}
                    ]
                }));
            })
            .await;

        let post_mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/pathprefix/Sessions/session-1/Playing");
                then.status(204);
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;
        let np = NowPlaying {
            session_id: Some("session-1".into()),
            ..np_default()
        };
        let result = Queue::append(&client, &np, &[(1, 2), (1, 3)]).await?;

        assert!(result.is_empty());
        post_mock.assert_calls_async(0).await;
        Ok(())
    }

    // An empty sessions array yields no NowPlaying events
    #[tokio::test]
    async fn empty_sessions() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/Sessions");
                then.json_body(serde_json::json!([]));
            })
            .await;

        let client = embyfin::Client::new(
            &server.url("/pathprefix"),
            "secret",
            embyfin::Fork::Jellyfin,
        )?;

        let sessions = client.sessions().await?;
        assert!(sessions.is_empty());

        sessions_mock.assert_async().await;

        Ok(())
    }
}
