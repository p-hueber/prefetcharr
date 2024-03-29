use std::collections::HashMap;

use anyhow::{anyhow, Result};
use reqwest::Url;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use super::{MediaServer, NowPlaying};

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
    now_playing_item: Episode,
    #[serde(flatten)]
    other: serde_json::Value,
}

pub enum Fork {
    Jellyfin,
    Emby,
}

pub struct Client {
    base_url: Url,
    api_key: String,
    fork: Fork,
}

impl Client {
    pub fn new(base_url: &str, api_key: String, fork: Fork) -> Result<Self> {
        let base_url = base_url.parse()?;
        Ok(Self {
            base_url,
            api_key,
            fork,
        })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let api_key_key = match self.fork {
            Fork::Jellyfin => "apikey",
            Fork::Emby => "api_key",
        };
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .extend(path.split('/'));
        url.query_pairs_mut()
            .append_pair(api_key_key, &self.api_key)
            .finish();
        let response = reqwest::get(url).await?;
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

impl MediaServer for Client {
    type Session = SessionInfo;
    type Error = anyhow::Error;

    async fn sessions(&self) -> std::prelude::v1::Result<Vec<Self::Session>, Self::Error> {
        Ok(self
            .get::<Vec<Value>>("Sessions")
            .await?
            .iter()
            .cloned()
            .map(serde_json::from_value)
            .filter_map(Result::ok)
            .collect::<Vec<Self::Session>>())
    }

    async fn extract(
        &self,
        session: Self::Session,
    ) -> std::prelude::v1::Result<NowPlaying, Self::Error> {
        let episode_num = session.now_playing_item.index_number;
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
        };

        Ok(now_playing)
    }
}
