use std::time::Duration;

use jellyfin_api::types::SessionInfo;
use tokio::sync::mpsc;

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

    pub async fn sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
        let sessions =
            reqwest::get(format!("{}Sessions?apikey={}", self.base_url, self.api_key)).await?;
        Ok(sessions.json::<Vec<SessionInfo>>().await?)
    }
}

pub async fn watch(interval: Duration, client: Client, tx: mpsc::Sender<Message>) {
    loop {
        if let Ok(sessions) = client.sessions().await {
            let playing = sessions
                .into_iter()
                .filter(|s| s.now_playing_item.is_some());
            for p in playing {
                tx.send(Message::NowPlaying(p))
                    .await
                    .expect("sending to event loop");
            }
        }
        tokio::time::sleep(interval).await;
    }
}
