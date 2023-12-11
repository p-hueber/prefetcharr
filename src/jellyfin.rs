use std::time::Duration;

use anyhow::{anyhow, Result};
use jellyfin_api::types::{BaseItemDto, SessionInfo};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::Message;

pub struct Client {
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(mut base_url: String, api_key: String) -> Self {
        if !base_url.ends_with("/") {
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
    user_id: Uuid,
    series_id: Uuid,
    season_id: Uuid,
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
            user_id,
            series_id,
            season_id,
        })
    }
}

#[derive(Debug)]
pub struct NowPlaying {
    pub tvdb: i32,
    pub episode: i32,
    pub season: i32,
}

pub async fn watch(interval: Duration, client: Client, tx: mpsc::Sender<Message>) {
    loop {
        if let Ok(sessions) = client.sessions().await {
            let playing = sessions
                .into_iter()
                .filter(|s| s.now_playing_item.is_some());
            for p in playing {
                match extract(p, &client).await {
                    Ok(now_playing) => {
                        tx.send(Message::NowPlaying(now_playing))
                            .await
                            .expect("sending to event loop");
                    }
                    Err(e) => eprintln!("Ignoring session: {e}"),
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

    let series = client.item(ids.user_id, ids.series_id).await?;

    let season = client.item(ids.user_id, ids.season_id).await?;
    let season_num = season.index_number.ok_or_else(|| anyhow!("no season"))?;

    let tvdb_id = series
        .provider_ids
        .as_ref()
        .and_then(|p| p.get("Tvdb"))
        .map(Option::as_ref)
        .flatten()
        .ok_or_else(|| anyhow!("no tmdb id"))?;

    let now_playing = NowPlaying {
        tvdb: tvdb_id.parse()?,
        episode: episode_num,
        season: season_num,
    };

    Ok(now_playing)
}
