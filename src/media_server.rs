use std::time::Duration;

use futures::{
    future::{BoxFuture, LocalBoxFuture},
    stream::{self, BoxStream, LocalBoxStream},
    FutureExt as _, StreamExt, TryStreamExt as _,
};
use tracing::debug;

use crate::util;

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
    pub user: User,
    pub library: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct User {
    pub name: String,
    pub id: String,
}

pub trait ProvideNowPlaying {
    type Session;

    fn sessions(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Self::Session>>> + Send;

    fn extract(
        &self,
        session: Self::Session,
    ) -> impl std::future::Future<Output = anyhow::Result<NowPlaying>> + Send;

    fn now_playing(&self) -> BoxFuture<anyhow::Result<BoxStream<NowPlaying>>>
    where
        <Self as ProvideNowPlaying>::Session: std::marker::Send,
        Self: Sync,
    {
        async move {
            let np = stream::iter(self.sessions().await?)
                .then(|s| self.extract(s))
                .inspect_err(|e| debug!("Ignoring session: {e}"))
                .filter_map(|r| async move { r.ok() })
                .boxed();
            Ok(np)
        }
        .boxed()
    }
}

pub trait Client {
    fn now_playing(&self) -> BoxFuture<anyhow::Result<BoxStream<NowPlaying>>>;

    fn now_playing_updates(
        &self,
        interval: Duration,
    ) -> LocalBoxStream<anyhow::Result<NowPlaying>> {
        stream::unfold(
            (self, Duration::new(0, 0)),
            move |(slf, delay)| async move {
                // first iteration has zero delay
                tokio::time::sleep(delay).await;

                let item = match slf.now_playing().await {
                    Ok(np_stream) => Ok(np_stream.map(Ok)),
                    Err(err) => Err(err),
                };
                Some((item, (slf, interval)))
            },
        )
        .try_flatten()
        .boxed_local()
    }

    fn probe(&self) -> LocalBoxFuture<anyhow::Result<()>>;

    fn probe_with_retry(&self, retries: usize) -> LocalBoxFuture<anyhow::Result<()>> {
        util::retry(retries, || self.probe()).boxed_local()
    }
}

#[cfg(test)]
pub mod test {
    use std::{future::ready, time::Instant};

    use anyhow::anyhow;
    use stream::TryStreamExt;

    use super::*;

    struct Mock {
        bad_session: bool,
        bad_call: bool,
    }

    impl Mock {
        fn new() -> Self {
            Self {
                bad_session: false,
                bad_call: false,
            }
        }
    }

    pub fn np_default() -> NowPlaying {
        NowPlaying {
            series: Series::Tvdb(1234),
            episode: 5,
            season: 3,
            user: User {
                name: "user".to_string(),
                id: "08ba1929-681e-4b24-929b-9245852f65c0".to_string(),
            },
            library: None,
        }
    }

    fn now_playing() -> Vec<NowPlaying> {
        vec![
            NowPlaying {
                series: Series::Tvdb(0),
                episode: 1,
                season: 2,
                ..np_default()
            },
            NowPlaying {
                series: Series::Tvdb(3),
                episode: 4,
                season: 5,
                ..np_default()
            },
        ]
    }

    impl ProvideNowPlaying for Mock {
        type Session = Option<NowPlaying>;

        async fn sessions(&self) -> anyhow::Result<Vec<Self::Session>> {
            let head = if self.bad_session {
                vec![None]
            } else {
                Vec::new()
            };
            let np = now_playing().into_iter().map(Some).collect();
            if self.bad_call {
                Err(anyhow!("API error"))
            } else {
                Ok([head, np].concat())
            }
        }

        async fn extract(&self, session: Self::Session) -> anyhow::Result<NowPlaying> {
            session.ok_or_else(|| anyhow!("no session"))
        }
    }

    impl Client for Mock {
        fn now_playing(&self) -> BoxFuture<anyhow::Result<BoxStream<NowPlaying>>> {
            ProvideNowPlaying::now_playing(self)
        }

        fn probe(&self) -> LocalBoxFuture<anyhow::Result<()>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn now_playing_first_instant() {
        let client = Mock::new();
        let mut np_updates = client.now_playing_updates(Duration::from_secs(3600));
        let expect = now_playing();
        let np: Vec<NowPlaying> = np_updates
            .as_mut()
            .take(expect.len())
            .try_collect()
            .await
            .unwrap();
        assert_eq!(np, expect);
        tokio::time::timeout(Duration::from_millis(100), np_updates.next())
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn now_playing_second_instant() {
        let client = Mock::new();
        let interval = Duration::from_millis(100);
        let np_updates = client.now_playing_updates(interval);
        let expect = now_playing();
        let earlier = Instant::now();
        let np: Vec<NowPlaying> = np_updates
            .skip(expect.len())
            .take(expect.len())
            .try_collect()
            .await
            .unwrap();
        assert!(Instant::now().duration_since(earlier) >= interval);
        assert_eq!(np, expect);
    }

    #[tokio::test]
    async fn now_playing_ignore_bad_session() {
        let client = Mock {
            bad_session: true,
            bad_call: false,
        };
        let interval = Duration::from_millis(100);
        let np_updates = client.now_playing_updates(interval);
        let expect = now_playing();
        let earlier = Instant::now();
        let np: Vec<NowPlaying> = np_updates
            .skip(expect.len())
            .take(expect.len())
            .try_collect()
            .await
            .unwrap();
        assert!(Instant::now().duration_since(earlier) >= interval);
        assert_eq!(np, expect);
    }

    #[tokio::test]
    async fn now_playing_ignore_bad_call() {
        let client = Mock {
            bad_session: true,
            bad_call: true,
        };
        let interval = Duration::from_millis(100);
        let np_updates = client.now_playing_updates(interval);
        let earlier = Instant::now();
        assert_eq!(
            np_updates
                .take(2)
                .filter(|i| ready(i.is_err()))
                .count()
                .await,
            2
        );
        assert!(Instant::now().duration_since(earlier) >= interval);
    }
}
