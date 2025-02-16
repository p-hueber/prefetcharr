use anyhow::{anyhow, bail, Result};
use futures::{
    future::{BoxFuture, LocalBoxFuture},
    stream::BoxStream,
    FutureExt,
};
use reqwest::header::{HeaderMap, HeaderValue};
use rustls_platform_verifier::ConfigVerifierExt;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;

use super::{NowPlaying, ProvideNowPlaying};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    id: String,
    title: String,
    #[serde(flatten)]
    _other: serde_json::Value,
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
    #[serde(flatten)]
    _other: serde_json::Value,
}

pub struct Client {
    http: reqwest::Client,
    url: reqwest::Url,
}

impl Client {
    pub fn new(url: &str, token: &str) -> Result<Self> {
        let mut token = HeaderValue::from_str(token)?;
        token.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("X-Plex-Token", token);
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .use_preconfigured_tls(rustls::ClientConfig::with_platform_verifier())
            .build()?;

        let url = url.parse()?;

        Ok(Self { http, url })
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
        Ok(NowPlaying {
            series,
            episode,
            season,
            user_id: session.user.id,
            user_name: session.user.title,
        })
    }
}

impl super::Client for Client {
    fn now_playing(&self) -> BoxFuture<anyhow::Result<BoxStream<NowPlaying>>> {
        ProvideNowPlaying::now_playing(self)
    }

    fn probe(&self) -> LocalBoxFuture<anyhow::Result<()>> {
        async {
            self.get::<Value>("status/sessions").await?;
            Ok(())
        }
        .boxed_local()
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use futures::StreamExt as _;

    use crate::media_server::{plex, Client, NowPlaying, Series};

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
                            "id": "1",
                            "title": "user",
                            "thumb": "ignore"
                        }
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

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Tvdb(1234),
            episode: 5,
            season: 3,
            user_id: "1".to_string(),
            user_name: "user".to_string(),
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;

        Ok(())
    }

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
                                    "User": {
                                        "id": "1",
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

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Tvdb(1234),
            episode: 5,
            season: 3,
            user_id: "1".to_string(),
            user_name: "user".to_string(),
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_async().await;

        Ok(())
    }

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
                                    "User": {
                                        "id": "1",
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

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Title("Test Show".to_string()),
            episode: 5,
            season: 3,
            user_id: "1".to_string(),
            user_name: "user".to_string(),
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;
        series_mock.assert_hits_async(0).await;

        Ok(())
    }
}
