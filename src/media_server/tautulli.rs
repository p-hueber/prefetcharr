use anyhow::{Context, Result, anyhow, bail};
use futures::{
    FutureExt,
    future::{BoxFuture, LocalBoxFuture},
    stream::BoxStream,
};
use reqwest::header::{HeaderMap, HeaderValue};
use rustls_platform_verifier::ConfigVerifierExt;
use serde::{Deserialize, Deserializer, de, de::DeserializeOwned};
use serde_json::Value;
use tracing::warn;

use super::{NowPlaying, ProvideNowPlaying};

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct User {
    #[serde(rename = "user_id")]
    id: i32,
    #[serde(rename = "username")]
    name: String,
}

impl From<User> for super::User {
    fn from(value: User) -> Self {
        Self {
            name: value.name,
            id: value.id.to_string(),
        }
    }
}

fn i32_from_string<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        String(String),
        Int(i32),
    }

    match StringOrInt::deserialize(deserializer)? {
        StringOrInt::String(s) => s.parse::<i32>().map_err(de::Error::custom),
        StringOrInt::Int(i) => Ok(i),
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct Episode {
    grandparent_title: String,
    grandparent_guids: Vec<String>,
    #[serde(deserialize_with = "i32_from_string")]
    media_index: i32,
    #[serde(deserialize_with = "i32_from_string")]
    parent_media_index: i32,
    media_type: String,
    #[serde(flatten)]
    user: User,
    library_name: String,
}

pub struct Client {
    http: reqwest::Client,
    url: reqwest::Url,
    apikey: String,
}

impl Client {
    pub fn new(url: &str, apikey: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
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
            apikey: apikey.to_string(),
        })
    }

    async fn get<T: DeserializeOwned>(&self, cmd: &str) -> Result<T> {
        let mut url = self.url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .extend(&["api", "v2"]);
        let response = self
            .http
            .get(url)
            .query(&[("apikey", &self.apikey), ("cmd", &cmd.to_string())])
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json::<T>().await?)
    }
}

impl ProvideNowPlaying for Client {
    type Session = Episode;

    async fn sessions(&self) -> anyhow::Result<Vec<Self::Session>> {
        let obj: serde_json::Map<String, Value> = self.get("get_activity").await?;

        let data = obj
            .get("response")
            .context("Tautulli response missing `response` field")?
            .get("data")
            .context("Tautulli response missing `data` field")?;

        let sessions = match data.get("sessions") {
            Some(sessions) => sessions
                .as_array()
                .context("Tautulli `sessions` field is not an array")?,
            None => return Ok(Vec::new()),
        };

        Ok(sessions
            .iter()
            .cloned()
            .map(serde_json::value::from_value)
            .inspect(|r| {
                if let Err(e) = r {
                    warn!(error = ?e, "Skipping Tautulli session due to deserialization error");
                }
            })
            .filter_map(Result::ok)
            .collect())
    }

    async fn extract(&self, session: Self::Session) -> anyhow::Result<NowPlaying> {
        if session.media_type != "episode" {
            bail!("not an episode");
        }
        let episode = session.media_index;
        let season = session.parent_media_index;

        let tvdb_id = session.grandparent_guids.iter().find_map(|uri| {
            let (provider, id) = uri.split_once("://")?;
            (provider == "tvdb").then_some(id.parse().ok()?)
        });
        let series = match tvdb_id {
            Some(id) => super::Series::Tvdb(id),
            None => super::Series::Title(session.grandparent_title),
        };
        let user = session.user.into();
        let library = Some(session.library_name);

        Ok(NowPlaying {
            series,
            episode,
            season,
            user,
            library,
        })
    }
}

impl super::Client for Client {
    fn now_playing(&self) -> BoxFuture<'_, anyhow::Result<BoxStream<'_, NowPlaying>>> {
        ProvideNowPlaying::now_playing(self)
    }

    fn probe(&self) -> LocalBoxFuture<'_, anyhow::Result<()>> {
        async {
            self.get::<Value>("get_activity").await?;
            Ok(())
        }
        .boxed_local()
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use futures::StreamExt as _;

    use crate::media_server::{
        Client, NowPlaying, ProvideNowPlaying, Series, tautulli, tautulli::Episode,
        test::np_default,
    };
    #[allow(clippy::unreadable_literal)]
    fn episode() -> serde_json::Value {
        serde_json::json!(
            {
                "response": {
                    "data": {
                        "sessions": [{
                            "grandparent_title": "Test Show",
                            "grandparent_guids": ["tvdb://1234"],
                            "media_index": "5",
                            "parent_media_index": "3",
                            "media_type": "episode",
                            "user_id": 29344801,
                            "username": "user",
                            "library_name": "TV Shows"
                        }]
                    }
                }
            }
        )
    }

    #[tokio::test]
    async fn probe() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v2");
                then.json_body(episode());
            })
            .await;

        let client = tautulli::Client::new(&server.url("/pathprefix"), "secret")?;
        client.probe().await?;

        sessions_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn extract() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v2");
                then.json_body(episode());
            })
            .await;

        let client = tautulli::Client::new(&server.url("/pathprefix"), "secret")?;
        let sessions = client.sessions().await?;
        let session = sessions.into_iter().next().unwrap();
        let extract = client.extract(session).await?;

        assert_eq!(
            extract,
            NowPlaying {
                series: Series::Tvdb(1234),
                episode: 5,
                season: 3,
                library: Some("TV Shows".into()),
                user: crate::media_server::User {
                    name: "user".into(),
                    id: "29344801".into(),
                },
            }
        );

        sessions_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn single_session() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v2");
                then.json_body(episode());
            })
            .await;

        let client = tautulli::Client::new(&server.url("/pathprefix"), "secret")?;
        let sessions = client.sessions().await?;

        assert_eq!(sessions.len(), 1);

        #[allow(clippy::unreadable_literal)]
        let session_expect = Episode {
            grandparent_title: "Test Show".into(),
            grandparent_guids: vec!["tvdb://1234".into()],
            media_index: 5,
            parent_media_index: 3,
            media_type: "episode".into(),
            user: tautulli::User {
                name: "user".into(),
                id: 29344801,
            },
            library_name: "TV Shows".into(),
        };

        assert_eq!(sessions[0], session_expect);

        sessions_mock.assert_async().await;

        Ok(())
    }
    #[tokio::test]
    async fn single_session_with_updates() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v2");
                then.json_body(episode());
            })
            .await;

        let client = tautulli::Client::new(&server.url("/pathprefix"), "secret")?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            library: Some("TV Shows".into()),
            user: crate::media_server::User {
                name: "user".into(),
                id: "29344801".into(),
            },
            ..np_default()
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn skip_invalid_sessions() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v2");
                #[allow(clippy::unreadable_literal)]
                then.json_body(serde_json::json!(
                    {
                        "response": {
                            "data": {
                                "sessions": [
                                    {   "invalid": "session" },
                                    {
                                        "grandparent_title": "invalid",
                                        "media_index": 5,
                                        "parent_media_index": 3,
                                        "media_type": "episode",
                                    },
                                    {
                                        "grandparent_title": "Test Show",
                                        "grandparent_guids": ["tvdb://1234"],
                                        "media_index": "5",
                                        "parent_media_index": "3",
                                        "media_type": "episode",
                                        "user_id": 29344801,
                                        "user_thumbnail": "ignore",
                                        "username": "user",
                                        "library_name": "TV Shows"
                                    }
                                ]
                            }
                        }
                    }
                ));
            })
            .await;

        let client = tautulli::Client::new(&server.url("/pathprefix"), "secret")?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            library: Some("TV Shows".into()),
            user: crate::media_server::User {
                name: "user".into(),
                id: "29344801".into(),
            },
            ..np_default()
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn name_fallback() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let sessions_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v2");
                #[allow(clippy::unreadable_literal)]
                then.json_body(serde_json::json!(
                      {
                        "response": {
                            "data": {
                                "sessions": [{
                                    "grandparent_title": "Test Show",
                                    "grandparent_guids": ["invalid"],
                                    "media_index": "5",
                                    "parent_media_index": "3",
                                    "media_type": "episode",
                                    "user_id": 29344801,
                                    "username": "user",
                                    "library_name": "TV Shows"
                                }]
                            }
                        }
                }
                    ));
            })
            .await;

        let client = tautulli::Client::new(&server.url("/pathprefix"), "secret")?;

        let mut np_updates = client.now_playing_updates(Duration::from_secs(100));
        let message = np_updates.next().await.transpose().unwrap();
        let message_expect = NowPlaying {
            series: Series::Title("Test Show".to_string()),
            library: Some("TV Shows".into()),
            user: crate::media_server::User {
                name: "user".into(),
                id: "29344801".into(),
            },
            ..np_default()
        };

        assert_eq!(message, Some(message_expect));

        sessions_mock.assert_async().await;

        Ok(())
    }
}
