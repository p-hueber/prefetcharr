use std::{collections::HashMap, time::Duration};

use anyhow::{anyhow, bail, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::debug;

use crate::Message;

use super::{NowPlaying, Series};

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
struct SessionInfo {
    user_id: Option<String>,
    now_playing_item: Option<BaseItemDto>,
    #[serde(flatten)]
    other: serde_json::Value,
}

pub struct Client {
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(mut base_url: String, api_key: String) -> Self {
        if !base_url.ends_with('/') {
            base_url += "/";
        }
        Self { base_url, api_key }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = reqwest::get(format!(
            "{}{}?api_key={}",
            self.base_url, path, self.api_key
        ))
        .await?;
        Ok(response.json::<T>().await?)
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        self.get("Sessions").await
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

pub async fn watch(interval: Duration, client: Client, tx: mpsc::Sender<Message>) {
    loop {
        if let Ok(sessions) = client.sessions().await {
            for p in sessions {
                match Box::pin(extract(p, &client)).await {
                    Ok(now_playing) => {
                        tx.send(Message::NowPlaying(now_playing))
                            .await
                            .expect("sending to event loop");
                    }
                    Err(e) => debug!("Ignoring session: {e}"),
                }
            }
        }
        tokio::time::sleep(interval).await;
    }
}

async fn extract(p: SessionInfo, client: &Client) -> Result<NowPlaying> {
    let episode_num = p
        .now_playing_item
        .as_ref()
        .and_then(|np| np.index_number)
        .ok_or_else(|| anyhow!("no episode"))?;
    let ids = Ids::try_from(p)?;

    let series = client.item(&ids.user, &ids.series).await?;

    let season = client.item(&ids.user, &ids.season).await?;
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
