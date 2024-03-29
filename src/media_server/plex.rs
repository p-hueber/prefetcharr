use anyhow::{anyhow, bail, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;

use super::{MediaServer, NowPlaying};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
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
}

impl MediaServer for Client {
    type Session = Episode;

    type Error = anyhow::Error;

    async fn sessions(&self) -> std::prelude::v1::Result<Vec<Self::Session>, Self::Error> {
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

    async fn extract(
        &self,
        session: Self::Session,
    ) -> std::prelude::v1::Result<NowPlaying, Self::Error> {
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
        })
    }
}
