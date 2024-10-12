use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::Message;

pub mod embyfin;
pub mod plex;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Series {
    Title(String),
    Tvdb(i32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NowPlaying {
    pub series: Series,
    pub episode: i32,
    pub season: i32,
    pub user_id: String,
    pub user_name: String,
}

pub trait MediaServer: Sized {
    type Session;
    type Error: std::fmt::Display;
    async fn sessions(&self) -> Result<Vec<Self::Session>, Self::Error>;
    async fn extract(&self, session: Self::Session) -> Result<NowPlaying, Self::Error>;
    async fn watch(self, interval: Duration, tx: mpsc::Sender<Message>) {
        loop {
            match self.sessions().await {
                Ok(sessions) => {
                    for session in sessions {
                        match self.extract(session).await {
                            Ok(now_playing) => {
                                tx.send(Message::NowPlaying(now_playing))
                                    .await
                                    .expect("sending to event loop");
                            }
                            Err(e) => debug!("Ignoring session: {e}"),
                        }
                    }
                }
                Err(err) => error!("cannot fetch sessions from media server: {err}"),
            }
            tokio::time::sleep(interval).await;
        }
    }
}
