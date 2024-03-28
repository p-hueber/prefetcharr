use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{error, trace};

use crate::Message;

use super::NowPlaying;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Episode {
    grandparent_title: String,
    grandparent_key: String,
    index: i32,
    parent_index: i32,
    r#type: String,
    #[serde(flatten)]
    _other: serde_json::Value,
}

pub struct Client {
    client: reqwest::Client,
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
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        let url = url.parse()?;

        Ok(Self { client, url })
    }

    async fn sessions(&self) -> Result<Vec<Episode>> {
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
                    .collect::<Vec<Episode>>()
            })
            .unwrap_or_default())
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut url = self.url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .extend(path.split('/'));
        let response = self.client.get(url).send().await?.error_for_status()?;
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

    async fn extract(&self, m: Episode) -> Result<NowPlaying> {
        if m.r#type != "episode" {
            bail!("not an episode");
        }
        let episode = m.index;
        let season = m.parent_index;
        let series = match self.tvdb(&m.grandparent_key).await {
            Some(id) => super::Series::Tvdb(id),
            None => super::Series::Title(m.grandparent_title),
        };
        Ok(NowPlaying {
            series,
            episode,
            season,
        })
    }

    pub async fn watch(&self, interval: Duration, tx: mpsc::Sender<Message>) {
        loop {
            match self.sessions().await {
                Ok(sessions) => {
                    for s in sessions {
                        trace!(?s, "processing session");
                        let Ok(np) = self.extract(s).await else {
                            continue;
                        };
                        tx.send(Message::NowPlaying(np)).await.ok();
                    }
                }
                Err(err) => error!("cannot fetch sessions from plex: {err}"),
            }
            tokio::time::sleep(interval).await;
        }
    }
}
