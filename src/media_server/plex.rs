use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::{
    FutureExt, StreamExt,
    future::{BoxFuture, LocalBoxFuture},
    stream::BoxStream,
};
use reqwest::header::{HeaderMap, HeaderValue};
use rustls_platform_verifier::ConfigVerifierExt;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::sync::{OnceCell, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use super::{NowPlaying, ProvideNowPlaying};

type PlayQueueCache = Arc<RwLock<HashMap<String, i64>>>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    id: String,
    title: String,
    #[serde(flatten)]
    _other: serde_json::Value,
}

impl From<User> for super::User {
    fn from(value: User) -> Self {
        Self {
            name: value.title,
            id: value.id,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    grandparent_title: String,
    grandparent_key: String,
    index: i32,
    parent_index: i32,
    r#type: String,
    #[serde(rename = "User")]
    user: User,
    library_section_title: String,
    #[serde(flatten)]
    other: serde_json::Value,
}

pub struct Client {
    http: reqwest::Client,
    url: reqwest::Url,
    token: String,
    machine_id: OnceCell<String>,
    play_queue_cache: PlayQueueCache,
}

impl Client {
    pub fn new(url: &str, token: &str) -> Result<Self> {
        let mut token_hv = HeaderValue::from_str(token)?;
        token_hv.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("X-Plex-Token", token_hv);
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .use_preconfigured_tls(rustls::ClientConfig::with_platform_verifier())
            .build()?;

        let url = url.parse()?;

        Ok(Self {
            http,
            url,
            token: token.to_string(),
            machine_id: OnceCell::new(),
            play_queue_cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Plex's `/status/sessions` REST endpoint does not include `playQueueID`,
    /// but the server pushes `PlaySessionStateNotification` over the WebSocket
    /// for every active session — that's where we discover the queue ID for a
    /// session, regardless of client (direct play or transcode).
    fn notifications_url(&self) -> String {
        let mut url = self.url.clone();
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        let _ = url.set_scheme(scheme);
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty()
                .extend([":", "websockets", "notifications"]);
        }
        url.query_pairs_mut()
            .append_pair("X-Plex-Token", &self.token);
        url.to_string()
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut url = self.url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .extend(path.split('/'));
        let response = self.http.get(url).send().await?.error_for_status()?;
        Ok(response.json::<T>().await?)
    }

    async fn tvdb(&self, key: &str) -> Option<i32> {
        self.get::<Value>(key)
            .await
            .ok()?
            .get("MediaContainer")?
            .get("Metadata")?
            .as_array()?
            .first()?
            .get("Guid")?
            .as_array()?
            .iter()
            .find_map(|g| {
                let uri = g.as_object()?.get("id")?.as_str()?;
                let (provider, id) = uri.split_once("://")?;
                (provider == "tvdb").then_some(id.parse().ok()?)
            })
    }
}

impl ProvideNowPlaying for Client {
    type Session = Episode;

    async fn sessions(&self) -> anyhow::Result<Vec<Self::Session>> {
        let obj: serde_json::Map<String, Value> = self.get("status/sessions").await?;
        Ok(obj
            .get("MediaContainer")
            .and_then(|v| v.get("Metadata"))
            .and_then(Value::as_array)
            .map(|metas| {
                metas
                    .iter()
                    .cloned()
                    .map(serde_json::value::from_value)
                    .filter_map(Result::ok)
                    .collect::<Vec<Self::Session>>()
            })
            .unwrap_or_default())
    }

    async fn extract(&self, session: Self::Session) -> anyhow::Result<NowPlaying> {
        if session.r#type != "episode" {
            bail!("not an episode");
        }
        let episode = session.index;
        let season = session.parent_index;
        let series = match self.tvdb(&session.grandparent_key).await {
            Some(id) => super::Series::Tvdb(id),
            None => super::Series::Title(session.grandparent_title),
        };
        let user = session.user.into();
        let library = Some(session.library_section_title);

        // Plex always exposes a top-level `sessionKey` in /status/sessions
        // metadata; `Session.id` is only populated for some session types
        // (typically those started via a transcoding decision). We key on
        // the always-present sessionKey so every active session is
        // trackable for the queue-append path.
        let session_id = session
            .other
            .get("sessionKey")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        Ok(NowPlaying {
            series,
            episode,
            season,
            user,
            library,
            session_id,
        })
    }
}

impl super::Client for Client {
    fn now_playing(&self) -> BoxFuture<'_, anyhow::Result<BoxStream<'_, NowPlaying>>> {
        ProvideNowPlaying::now_playing(self)
    }

    fn probe(&self) -> LocalBoxFuture<'_, anyhow::Result<()>> {
        async {
            self.get::<Value>("status/sessions").await?;
            Ok(())
        }
        .boxed_local()
    }

    fn run(&self) -> BoxFuture<'_, ()> {
        let url = self.notifications_url();
        let cache = self.play_queue_cache.clone();
        async move { notifications_loop(url, cache).await }.boxed()
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

            let raw: Value = self.get("status/sessions").await?;
            let metadata = raw
                .get("MediaContainer")
                .and_then(|m| m.get("Metadata"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            let session = metadata
                .into_iter()
                .find(|m| m.get("sessionKey").and_then(Value::as_str) == Some(session_id))
                .ok_or_else(|| anyhow!("session {session_id} no longer exists"))?;

            let Some(play_queue_id) = self.play_queue_cache.read().await.get(session_id).copied()
            else {
                debug!(
                    session = session_id,
                    "playQueueID not yet observed via WebSocket; will retry next poll"
                );
                return Ok(HashSet::new());
            };

            let series_key = session
                .get("grandparentKey")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("session has no grandparentKey"))?
                .to_string();

            let machine_id = self.machine_identifier().await?;
            let episode_map = self.list_series_episodes(&series_key).await?;
            let existing = self.play_queue_rating_keys(play_queue_id).await?;

            let mut success: HashSet<(i32, i32)> = HashSet::new();
            let mut to_put: Vec<((i32, i32), String)> = Vec::new();
            // Episodes arrive in playback order; stop at the first gap
            // (episode not yet in the library) so we never enqueue an episode
            // whose predecessor is still missing.
            for &(s, e) in &episodes {
                let Some(rk) = episode_map.get(&(s, e)) else {
                    debug!(season = s, episode = e, "episode not yet in library; stop");
                    break;
                };
                if existing.contains(rk) {
                    debug!(season = s, episode = e, "episode already in queue");
                    success.insert((s, e));
                } else {
                    to_put.push(((s, e), rk.clone()));
                }
            }

            for ((s, e), rk) in &to_put {
                let mut url = self.url.clone();
                url.path_segments_mut()
                    .map_err(|()| anyhow!("url is relative"))?
                    .extend(["playQueues", &play_queue_id.to_string()]);
                let uri = format!(
                    "server://{machine_id}/com.plexapp.plugins.library/library/metadata/{rk}"
                );
                url.query_pairs_mut().append_pair("uri", &uri);
                self.http
                    .put(url)
                    .send()
                    .await?
                    .error_for_status()
                    .with_context(|| format!("PUT /playQueues/{play_queue_id}"))?;
                info!("appended s{s:02}e{e:02} to play queue");
                success.insert((*s, *e));
            }

            Ok(success)
        }
        .boxed()
    }
}

impl Client {
    async fn machine_identifier(&self) -> Result<String> {
        self.machine_id
            .get_or_try_init(|| async {
                let identity: Value = self.get("identity").await?;
                identity
                    .get("MediaContainer")
                    .and_then(|m| m.get("machineIdentifier"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("missing machineIdentifier in /identity"))
            })
            .await
            .cloned()
    }

    async fn list_series_episodes(&self, series_key: &str) -> Result<HashMap<(i32, i32), String>> {
        let path = format!("{}/allLeaves", series_key.trim_start_matches('/'));
        let leaves: Value = self.get(&path).await?;
        let mut map = HashMap::new();
        if let Some(arr) = leaves
            .get("MediaContainer")
            .and_then(|m| m.get("Metadata"))
            .and_then(Value::as_array)
        {
            for item in arr {
                let s = item
                    .get("parentIndex")
                    .and_then(Value::as_i64)
                    .and_then(|x| i32::try_from(x).ok());
                let e = item
                    .get("index")
                    .and_then(Value::as_i64)
                    .and_then(|x| i32::try_from(x).ok());
                let key = item
                    .get("ratingKey")
                    .and_then(Value::as_str)
                    .map(String::from);
                if let (Some(s), Some(e), Some(k)) = (s, e, key) {
                    map.insert((s, e), k);
                }
            }
        }
        Ok(map)
    }

    #[cfg(test)]
    fn play_queue_cache(&self) -> &PlayQueueCache {
        &self.play_queue_cache
    }

    async fn play_queue_rating_keys(&self, play_queue_id: i64) -> Result<HashSet<String>> {
        let path = format!("playQueues/{play_queue_id}");
        let queue: Value = self.get(&path).await?;
        Ok(queue
            .get("MediaContainer")
            .and_then(|m| m.get("Metadata"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|i| i.get("ratingKey").and_then(Value::as_str).map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }
}

async fn notifications_loop(url: String, cache: PlayQueueCache) {
    let mut delay = Duration::from_secs(1);
    loop {
        match run_notifications(&url, &cache).await {
            Ok(()) => {
                debug!("plex notifications stream ended; reconnecting");
                delay = Duration::from_secs(1);
            }
            Err(e) => {
                warn!("plex notifications stream error: {e:#}");
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
        tokio::time::sleep(delay).await;
    }
}

async fn run_notifications(url: &str, cache: &PlayQueueCache) -> Result<()> {
    let (mut ws, _resp) = connect_async(url).await.context("ws connect")?;
    while let Some(msg) = ws.next().await {
        match msg.context("ws recv")? {
            Message::Text(text) => {
                if let Ok(v) = serde_json::from_str::<Value>(text.as_str()) {
                    update_play_queue_cache(&v, cache).await;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}

async fn update_play_queue_cache(v: &Value, cache: &PlayQueueCache) {
    let Some(arr) = v
        .get("NotificationContainer")
        .and_then(|nc| nc.get("PlaySessionStateNotification"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let mut guard = cache.write().await;
    for n in arr {
        let Some(session_key) = n.get("sessionKey").and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_i64().map(|i| i.to_string()))
        }) else {
            continue;
        };
        let state = n.get("state").and_then(Value::as_str).unwrap_or("");
        if state == "stopped" {
            guard.remove(&session_key);
            continue;
        }
        if let Some(pqid) = n.get("playQueueID").and_then(Value::as_i64) {
            guard.insert(session_key, pqid);
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use futures::StreamExt as _;

    use std::collections::HashSet;

    use crate::media_server::{
        Client, NowPlaying, ProvideNowPlaying, Queue, Series, plex,
        test::{idle_pending, np_default},
    };

    fn episode() -> serde_json::Value {
        serde_json::json!(
            {
                "MediaContainer": {
                    "Metadata": [{
                        "grandparentTitle": "Test Show",
                        "grandparentKey": "path/to/series",
                        "index": 5,
                        "parentIndex": 3,
                        "type": "episode",
                        "User": {
                            "id": "08ba1929-681e-4b24-929b-9245852f65c0",
                            "title": "user",
                            "thumb": "ignore"
                        },
                        "librarySectionTitle": "TV Shows"
                    }]
                }
            }
        )
    }

    fn series() -> serde_json::Value {
        serde_json::json!({
            "MediaContainer": {
                "Metadata": [{
                    "Guid": [
                        {"id": "ignore"},
                        {"id": "ignore://"},
                        {"id": "://ignore"},
                        {"id": "ignore://0"},
                        {"id": "tvdb://1234"}
                    ]
                }]
            }
        })
    }

    // Extracts NowPlaying from a valid Plex session, resolving the series via TVDB lookup
    #[tokio::test]
    async fn single_session() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/status/sessions");
                then.json_body(episode());
            })
            .await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/path/to/series");
                then.json_body(series());
            })
            .await;

        let client = plex::Client::new(&server.url("/pathprefix"), "secret")?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100), idle_pending());
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            library: Some("TV Shows".into()),
            ..np_default()
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;

        Ok(())
    }

    // Invalid and incomplete sessions are skipped, valid sessions still extracted
    #[tokio::test]
    async fn skip_invalid_sessions() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/status/sessions");
                then.json_body(serde_json::json!(
                    {
                        "MediaContainer": {
                            "Metadata": [
                                { "invalid": "session" },
                                {
                                    "grandparentTitle": "invalid",
                                    "index": 5,
                                    "parentIndex": 3,
                                    "type": "episode"
                                },
                                {
                                    "grandparentTitle": "Test Show",
                                    "grandparentKey": "path/to/series",
                                    "index": 5,
                                    "parentIndex": 3,
                                    "type": "invalid"
                                },
                                {
                                    "grandparentTitle": "Test Show",
                                    "grandparentKey": "path/to/series",
                                    "index": 5,
                                    "parentIndex": 3,
                                    "type": "episode",
                                    "librarySectionTitle": "TV Shows",
                                    "User": {
                                        "id": "08ba1929-681e-4b24-929b-9245852f65c0",
                                        "title": "user",
                                        "thumb": "ignore"
                                    }
                                }
                            ]
                        }
                    }
                ));
            })
            .await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/path/to/series");
                then.json_body(series());
            })
            .await;

        let client = plex::Client::new(&server.url("/pathprefix"), "secret")?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100), idle_pending());
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            library: Some("TV Shows".into()),
            ..np_default()
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;

        Ok(())
    }

    // Falls back to series title when the TVDB metadata lookup fails
    #[tokio::test]
    async fn name_fallback() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/status/sessions");
                then.json_body(serde_json::json!(
                    {
                        "MediaContainer": {
                            "Metadata": [
                                {
                                    "grandparentTitle": "Test Show",
                                    "grandparentKey": "invalid",
                                    "index": 5,
                                    "parentIndex": 3,
                                    "type": "episode",
                                    "librarySectionTitle": "TV Shows",
                                    "User": {
                                        "id": "08ba1929-681e-4b24-929b-9245852f65c0",
                                        "title": "user",
                                        "thumb": "ignore"
                                    }
                                }
                            ]
                        }
                    }
                ));
            })
            .await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/path/to/series");
                then.json_body(series());
            })
            .await;

        let client = plex::Client::new(&server.url("/pathprefix"), "secret")?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100), idle_pending());
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Title("Test Show".to_string()),
            library: Some("TV Shows".into()),
            ..np_default()
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_calls_async(0).await;

        Ok(())
    }

    fn sessions_for_queue() -> serde_json::Value {
        serde_json::json!({
            "MediaContainer": {
                "Metadata": [
                    {
                        "grandparentTitle": "Test Show",
                        "grandparentKey": "/library/metadata/100",
                        "index": 1,
                        "parentIndex": 1,
                        "type": "episode",
                        "sessionKey": "session-1",
                        "User": {
                            "id": "08ba1929-681e-4b24-929b-9245852f65c0",
                            "title": "user",
                            "thumb": "ignore"
                        },
                        "librarySectionTitle": "TV Shows"
                    }
                ]
            }
        })
    }

    fn series_leaves() -> serde_json::Value {
        serde_json::json!({
            "MediaContainer": {
                "Metadata": [
                    {"ratingKey": "201", "parentIndex": 1, "index": 1},
                    {"ratingKey": "202", "parentIndex": 1, "index": 2},
                    {"ratingKey": "203", "parentIndex": 1, "index": 3}
                ]
            }
        })
    }

    fn identity() -> serde_json::Value {
        serde_json::json!({
            "MediaContainer": {"machineIdentifier": "machine-1"}
        })
    }

    // Plex Queue::append issues a PUT per new episode with the correct server URI
    #[tokio::test]
    async fn queue_append_success() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let _sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/status/sessions");
                then.json_body(sessions_for_queue());
            })
            .await;

        let _identity_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/identity");
                then.json_body(identity());
            })
            .await;

        let _leaves_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/library/metadata/100/allLeaves");
                then.json_body(series_leaves());
            })
            .await;

        let _queue_mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/pathprefix/playQueues/99");
                then.json_body(serde_json::json!({
                    "MediaContainer": {"Metadata": [
                        {"ratingKey": "201"}
                    ]}
                }));
            })
            .await;

        let put_e2 = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::PUT)
                    .path("/pathprefix/playQueues/99")
                    .query_param(
                        "uri",
                        "server://machine-1/com.plexapp.plugins.library/library/metadata/202",
                    );
                then.status(200).json_body(serde_json::json!({}));
            })
            .await;
        let put_e3 = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::PUT)
                    .path("/pathprefix/playQueues/99")
                    .query_param(
                        "uri",
                        "server://machine-1/com.plexapp.plugins.library/library/metadata/203",
                    );
                then.status(200).json_body(serde_json::json!({}));
            })
            .await;

        let client = plex::Client::new(&server.url("/pathprefix"), "secret")?;
        // Seed the play queue cache as if a WS notification had populated it.
        client
            .play_queue_cache()
            .write()
            .await
            .insert("session-1".into(), 99);
        let np = NowPlaying {
            session_id: Some("session-1".into()),
            ..np_default()
        };
        let result = Queue::append(&client, &np, &[(1, 2), (1, 3)]).await?;

        let expected: HashSet<(i32, i32)> = [(1, 2), (1, 3)].into_iter().collect();
        assert_eq!(result, expected);

        put_e2.assert_async().await;
        put_e3.assert_async().await;
        Ok(())
    }

    // When the WS notification has not yet observed the playQueueID for an
    // active session, append returns an empty success set and is retried on
    // a future poll cycle (driven by the Actor's pending store).
    #[tokio::test]
    async fn queue_append_no_op_without_play_queue_id() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let _sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/status/sessions");
                then.json_body(sessions_for_queue());
            })
            .await;

        let put_any = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::PUT)
                    .path("/pathprefix/playQueues/99");
                then.status(200).json_body(serde_json::json!({}));
            })
            .await;

        let client = plex::Client::new(&server.url("/pathprefix"), "secret")?;
        // No cache seeding — playQueueID is unknown.
        let np = NowPlaying {
            session_id: Some("session-1".into()),
            ..np_default()
        };
        let result = Queue::append(&client, &np, &[(1, 2), (1, 3)]).await?;

        assert!(result.is_empty());
        put_any.assert_calls_async(0).await;
        Ok(())
    }

    // Episodes already in the active playqueue are reported as success without
    // issuing PUT requests
    #[tokio::test]
    async fn queue_append_dedup_existing() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let _sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/status/sessions");
                then.json_body(sessions_for_queue());
            })
            .await;
        let _identity_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/identity");
                then.json_body(identity());
            })
            .await;
        let _leaves_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/library/metadata/100/allLeaves");
                then.json_body(series_leaves());
            })
            .await;
        let _queue_mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/pathprefix/playQueues/99");
                then.json_body(serde_json::json!({
                    "MediaContainer": {"Metadata": [
                        {"ratingKey": "201"},
                        {"ratingKey": "202"}
                    ]}
                }));
            })
            .await;
        let put_e3 = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::PUT)
                    .path("/pathprefix/playQueues/99")
                    .query_param(
                        "uri",
                        "server://machine-1/com.plexapp.plugins.library/library/metadata/203",
                    );
                then.status(200).json_body(serde_json::json!({}));
            })
            .await;

        let client = plex::Client::new(&server.url("/pathprefix"), "secret")?;
        client
            .play_queue_cache()
            .write()
            .await
            .insert("session-1".into(), 99);
        let np = NowPlaying {
            session_id: Some("session-1".into()),
            ..np_default()
        };
        let result = Queue::append(&client, &np, &[(1, 2), (1, 3)]).await?;

        let expected: HashSet<(i32, i32)> = [(1, 2), (1, 3)].into_iter().collect();
        assert_eq!(result, expected);
        put_e3.assert_async().await;
        Ok(())
    }

    // If the first episode in playback order is missing from the library, no
    // PUTs are issued — we never want to skip ahead past a gap.
    #[tokio::test]
    async fn queue_append_stops_at_leading_gap() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let _sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/status/sessions");
                then.json_body(sessions_for_queue());
            })
            .await;
        let _identity_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/identity");
                then.json_body(identity());
            })
            .await;
        // Library has s01e01 (currently playing) and s01e03 but not s01e02.
        let _leaves_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/library/metadata/100/allLeaves");
                then.json_body(serde_json::json!({
                    "MediaContainer": {
                        "Metadata": [
                            {"ratingKey": "201", "parentIndex": 1, "index": 1},
                            {"ratingKey": "203", "parentIndex": 1, "index": 3}
                        ]
                    }
                }));
            })
            .await;
        let _queue_mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/pathprefix/playQueues/99");
                then.json_body(serde_json::json!({
                    "MediaContainer": {"Metadata": [
                        {"ratingKey": "201"}
                    ]}
                }));
            })
            .await;
        let put_any = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::PUT)
                    .path("/pathprefix/playQueues/99");
                then.status(200).json_body(serde_json::json!({}));
            })
            .await;

        let client = plex::Client::new(&server.url("/pathprefix"), "secret")?;
        client
            .play_queue_cache()
            .write()
            .await
            .insert("session-1".into(), 99);
        let np = NowPlaying {
            session_id: Some("session-1".into()),
            ..np_default()
        };
        let result = Queue::append(&client, &np, &[(1, 2), (1, 3)]).await?;

        assert!(result.is_empty());
        put_any.assert_calls_async(0).await;
        Ok(())
    }

    // PlaySessionStateNotification frames update the cache; a `stopped` frame
    // evicts the session.
    #[tokio::test]
    async fn ws_cache_handles_play_and_stop_notifications() {
        use std::sync::Arc;
        use tokio::sync::RwLock;
        let cache: super::PlayQueueCache = Arc::new(RwLock::new(std::collections::HashMap::new()));

        let playing = serde_json::json!({
            "NotificationContainer": {
                "type": "playing",
                "PlaySessionStateNotification": [{
                    "sessionKey": "3",
                    "playQueueID": 18,
                    "state": "playing"
                }]
            }
        });
        super::update_play_queue_cache(&playing, &cache).await;
        assert_eq!(cache.read().await.get("3"), Some(&18));

        let stopped = serde_json::json!({
            "NotificationContainer": {
                "type": "playing",
                "PlaySessionStateNotification": [{
                    "sessionKey": "3",
                    "state": "stopped"
                }]
            }
        });
        super::update_play_queue_cache(&stopped, &cache).await;
        assert!(cache.read().await.get("3").is_none());
    }

    // A session with all required fields but a non-episode type is rejected by extract
    #[tokio::test]
    async fn extract_rejects_non_episode() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.path("/pathprefix/status/sessions");
                then.json_body(serde_json::json!(
                    {
                        "MediaContainer": {
                            "Metadata": [{
                                "grandparentTitle": "Some Movie",
                                "grandparentKey": "path/to/series",
                                "index": 0,
                                "parentIndex": 0,
                                "type": "movie",
                                "User": {
                                    "id": "08ba1929-681e-4b24-929b-9245852f65c0",
                                    "title": "user",
                                    "thumb": "ignore"
                                },
                                "librarySectionTitle": "Movies"
                            }]
                        }
                    }
                ));
            })
            .await;

        let client = plex::Client::new(&server.url("/pathprefix"), "secret")?;
        let session = client.sessions().await?.into_iter().next().unwrap();
        assert!(client.extract(session).await.is_err());

        Ok(())
    }
}
