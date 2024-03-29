use std::time::Duration;

use anyhow::{anyhow, Result};
use jellyfin_api::types::{BaseItemDto, SessionInfo};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tracing::debug;
use uuid::Uuid;

use crate::Message;

use super::NowPlaying;

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
        let response =
            reqwest::get(format!("{}{}?apikey={}", self.base_url, path, self.api_key)).await?;
        Ok(response.json::<T>().await?)
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        self.get("Sessions").await
    }

    async fn item(&self, user_id: Uuid, item_id: Uuid) -> Result<BaseItemDto> {
        let path = format!("Users/{user_id}/Items/{item_id}");
        self.get(path.as_str()).await
    }
}

#[derive(Debug)]
struct Ids {
    user: Uuid,
    series: Uuid,
    season: Uuid,
}

impl TryFrom<SessionInfo> for Ids {
    type Error = anyhow::Error;

    fn try_from(session: SessionInfo) -> std::result::Result<Self, Self::Error> {
        Ids::new(&session)
    }
}

impl Ids {
    fn new(session: &SessionInfo) -> Result<Self> {
        let user_id = session.user_id.ok_or_else(|| anyhow!("missing user_id"))?;
        let np = session
            .now_playing_item
            .as_ref()
            .ok_or_else(|| anyhow!("missing now_playing_item"))?;
        let series_id = np.series_id.ok_or_else(|| anyhow!("missing series_id"))?;
        let season_id = np.season_id.ok_or_else(|| anyhow!("missing season_id"))?;

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
            let playing = sessions
                .into_iter()
                .filter(|s| s.now_playing_item.is_some());
            for p in playing {
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

    let series = client.item(ids.user, ids.series).await?;

    let season = client.item(ids.user, ids.season).await?;
    let season_num = season.index_number.ok_or_else(|| anyhow!("no season"))?;

    let tvdb_id = series
        .provider_ids
        .as_ref()
        .and_then(|p| p.get("Tvdb"))
        .and_then(Option::as_ref)
        .ok_or_else(|| anyhow!("no tmdb id"))?;

    let now_playing = NowPlaying {
        tvdb: tvdb_id.parse()?,
        episode: episode_num,
        season: season_num,
    };

    Ok(now_playing)
}
