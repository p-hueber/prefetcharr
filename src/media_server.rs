use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{
    FutureExt as _, StreamExt, TryStreamExt as _,
    future::{BoxFuture, LocalBoxFuture, pending},
    stream::{self, BoxStream, LocalBoxStream},
};
use tracing::debug;

use crate::util;

pub mod embyfin;
pub mod plex;
pub mod tautulli;

pub const ACTIVE_POLL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Series {
    Title(String),
    Tvdb(i32),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NowPlaying {
    pub series: Series,
    pub episode: i32,
    pub season: i32,
    pub user: User,
    pub library: Option<String>,
    pub session_id: Option<String>,
}

// Identity used for prefetch-side dedup. Excludes `session_id` so the same
// episode is prefetched at most once across sessions and restarts.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PrefetchKey {
    pub series: Series,
    pub episode: i32,
    pub season: i32,
    pub user: User,
    pub library: Option<String>,
}

impl From<&NowPlaying> for PrefetchKey {
    fn from(np: &NowPlaying) -> Self {
        Self {
            series: np.series.clone(),
            episode: np.episode,
            season: np.season,
            user: np.user.clone(),
            library: np.library.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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

    fn now_playing(&self) -> BoxFuture<'_, anyhow::Result<BoxStream<'_, NowPlaying>>>
    where
        <Self as ProvideNowPlaying>::Session: std::marker::Send,
        Self: Sync,
    {
        async move {
            let np = stream::iter(self.sessions().await?)
                .then(|s| self.extract(s))
                .inspect_err(|e| debug!("Ignoring session: {e}"))
                .filter_map(async |r| r.ok())
                .boxed();
            Ok(np)
        }
        .boxed()
    }
}

pub trait Queue {
    // Append the requested (season, episode) pairs to the play queue of the
    // session referenced by `np`. Returns the subset successfully appended or
    // already present in the queue. A backend-level error (network, auth)
    // returns `Err`; partial resolution during normal operation returns Ok.
    fn append(
        &self,
        np: &NowPlaying,
        episodes: &[(i32, i32)],
    ) -> BoxFuture<'_, anyhow::Result<HashSet<(i32, i32)>>>;
}

pub trait Client {
    fn now_playing(&self) -> BoxFuture<'_, anyhow::Result<BoxStream<'_, NowPlaying>>>;

    fn now_playing_updates(
        &self,
        interval: Duration,
        has_pending: Arc<AtomicBool>,
    ) -> LocalBoxStream<'_, anyhow::Result<NowPlaying>> {
        stream::unfold(
            (self, interval, has_pending, true),
            async |(slf, interval, has_pending, first)| {
                if !first {
                    // `has_pending` is set by the actor task *after* it
                    // processes a NowPlaying message, so checking it once
                    // up-front races: at the moment the previous iteration's
                    // emit returns, the actor likely hasn't yet flipped the
                    // flag. Sleep in `ACTIVE_POLL_INTERVAL` chunks instead and
                    // re-check after each chunk so a flip mid-wait shortens
                    // the wait to at most one chunk.
                    let deadline = tokio::time::Instant::now() + interval;
                    loop {
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        let chunk = remaining.min(ACTIVE_POLL_INTERVAL);
                        if chunk.is_zero() {
                            break;
                        }
                        tokio::time::sleep(chunk).await;
                        if has_pending.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }

                let item = match slf.now_playing().await {
                    Ok(np_stream) => Ok(np_stream.map(Ok)),
                    Err(err) => Err(err),
                };
                Some((item, (slf, interval, has_pending, false)))
            },
        )
        .try_flatten()
        .boxed_local()
    }

    fn probe(&self) -> LocalBoxFuture<'_, anyhow::Result<()>>;

    fn run(&self) -> BoxFuture<'_, ()> {
        pending().boxed()
    }

    fn probe_with_retry(&self, retries: usize) -> LocalBoxFuture<'_, anyhow::Result<()>> {
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
            session_id: None,
        }
    }

    pub fn idle_pending() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
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
        fn now_playing(&self) -> BoxFuture<'_, anyhow::Result<BoxStream<'_, NowPlaying>>> {
            ProvideNowPlaying::now_playing(self)
        }

        fn probe(&self) -> LocalBoxFuture<'_, anyhow::Result<()>> {
            unimplemented!()
        }
    }

    // First poll returns results immediately without waiting for the interval
    #[tokio::test]
    async fn now_playing_first_instant() {
        let client = Mock::new();
        let mut np_updates = client.now_playing_updates(Duration::from_secs(3600), idle_pending());
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

    // Second poll waits for the configured interval before returning
    #[tokio::test]
    async fn now_playing_second_instant() {
        let client = Mock::new();
        let interval = Duration::from_millis(100);
        let np_updates = client.now_playing_updates(interval, idle_pending());
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

    // Bad sessions are silently filtered out, valid sessions still returned
    #[tokio::test]
    async fn now_playing_ignore_bad_session() {
        let client = Mock {
            bad_session: true,
            bad_call: false,
        };
        let interval = Duration::from_millis(100);
        let np_updates = client.now_playing_updates(interval, idle_pending());
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

    // Failed API calls propagate as errors in the update stream
    #[tokio::test]
    async fn now_playing_ignore_bad_call() {
        let client = Mock {
            bad_session: true,
            bad_call: true,
        };
        let interval = Duration::from_millis(100);
        let np_updates = client.now_playing_updates(interval, idle_pending());
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

    // `has_pending` flipping after the polling loop entered its sleep must
    // shorten the wait. Without the chunked recheck, the poller would sleep
    // the full `interval` because the flag is read once up-front.
    #[tokio::test(start_paused = true)]
    async fn fast_poll_when_pending_flips_during_sleep() {
        let client = Mock::new();
        let interval = Duration::from_secs(900);
        let pending = Arc::new(AtomicBool::new(false));
        let mut np_updates = client.now_playing_updates(interval, pending.clone());

        for _ in 0..now_playing().len() {
            np_updates.next().await.unwrap().unwrap();
        }

        let p = pending.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            p.store(true, Ordering::Relaxed);
        });

        let start = tokio::time::Instant::now();
        np_updates.next().await.unwrap().unwrap();
        let elapsed = tokio::time::Instant::now().duration_since(start);
        assert!(
            elapsed <= ACTIVE_POLL_INTERVAL,
            "elapsed {elapsed:?}, expected ≤ {ACTIVE_POLL_INTERVAL:?}",
        );
    }
}
