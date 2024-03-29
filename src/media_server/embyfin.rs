use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::{MediaServer, NowPlaying, Series};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BaseItemDto {
    name: Option<String>,
    series_id: Option<String>,
    season_id: Option<String>,
    index_number: Option<i32>,
    provider_ids: HashMap<String, String>,
    #[serde(flatten)]
    other: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SessionInfo {
    user_id: Option<String>,
    now_playing_item: Option<BaseItemDto>,
    #[serde(flatten)]
    other: serde_json::Value,
}

pub enum Fork {
    Jellyfin,
    Emby,
}

pub struct Client {
    base_url: String,
    api_key: String,
    fork: Fork,
}

impl Client {
    pub fn new(mut base_url: String, api_key: String, fork: Fork) -> Self {
        if !base_url.ends_with('/') {
            base_url += "/";
        }
        Self {
            base_url,
            api_key,
            fork,
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let api_key_key = match self.fork {
            Fork::Jellyfin => "apikey",
            Fork::Emby => "api_key",
        };
        let response = reqwest::get(format!(
            "{}{}?{}={}",
            self.base_url, path, api_key_key, self.api_key
        ))
        .await?;
        Ok(response.json::<T>().await?)
    }

    async fn item(&self, user_id: &str, item_id: &str) -> Result<BaseItemDto> {
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

impl TryFrom<SessionInfo> for Ids {
    type Error = anyhow::Error;

    fn try_from(session: SessionInfo) -> std::result::Result<Self, Self::Error> {
        Ids::new(&session)
    }
}

impl Ids {
    fn new(session: &SessionInfo) -> Result<Self> {
        let user_id = session
            .user_id
            .clone()
            .ok_or_else(|| anyhow!("missing user_id"))?;
        let np = session
            .now_playing_item
            .as_ref()
            .ok_or_else(|| anyhow!("missing now_playing_item"))?;
        let series_id = np
            .series_id
            .clone()
            .ok_or_else(|| anyhow!("missing series_id"))?;
        let season_id = np
            .season_id
            .clone()
            .ok_or_else(|| anyhow!("missing season_id"))?;

        Ok(Self {
            user: user_id,
            series: series_id,
            season: season_id,
        })
    }
}

impl MediaServer for Client {
    type Session = SessionInfo;
    type Error = anyhow::Error;

    async fn sessions(&self) -> std::prelude::v1::Result<Vec<Self::Session>, Self::Error> {
        self.get("Sessions").await
    }

    async fn extract(
        &self,
        session: Self::Session,
    ) -> std::prelude::v1::Result<NowPlaying, Self::Error> {
        let episode_num = session
            .now_playing_item
            .as_ref()
            .and_then(|np| np.index_number)
            .ok_or_else(|| anyhow!("no episode"))?;
        let ids = Ids::try_from(session)?;

        let series = self.item(&ids.user, &ids.series).await?;

        let season = self.item(&ids.user, &ids.season).await?;
        let season_num = season.index_number.ok_or_else(|| anyhow!("no season"))?;

        let tvdb_id = series.provider_ids.get("Tvdb");

        let series = match (tvdb_id, series.name) {
            (Some(tvdb), _) => Series::Tvdb(tvdb.parse()?),
            (None, Some(title)) => Series::Title(title),
            (None, None) => bail!("no tmdb id or name"),
        };

        let now_playing = NowPlaying {
            series,
            episode: episode_num,
            season: season_num,
        };

        Ok(now_playing)
    }
}
