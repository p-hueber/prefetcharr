use anyhow::anyhow;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::{
    media_server::{NowPlaying, Series},
    once::Seen,
    sonarr, Message,
};

pub struct Actor {
    rx: mpsc::Receiver<Message>,
    sonarr_client: sonarr::Client,
    seen: Seen,
    remaining_episodes: u8,
}

impl Actor {
    pub fn new(
        rx: mpsc::Receiver<Message>,
        sonarr_client: sonarr::Client,
        seen: Seen,
        remaining_episodes: u8,
    ) -> Self {
        Self {
            rx,
            sonarr_client,
            seen,
            remaining_episodes,
        }
    }
}

impl Actor {
    pub async fn process(&mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                Message::NowPlaying(np) => {
                    if let Err(e) = self.search_next(np).await {
                        error!(err = ?e, "Failed to process");
                    }
                }
            };
        }
    }

    async fn search_next(&mut self, np: NowPlaying) -> anyhow::Result<()> {
        let series = self.sonarr_client.series().await?;
        let mut series = series
            .into_iter()
            .find(|s| match &np.series {
                Series::Title(t) => s.title.as_ref() == Some(t),
                Series::Tvdb(i) => &s.tvdb_id == i,
            })
            .ok_or_else(|| anyhow!("series not found in Sonarr"))?;

        info!(title = series.title.clone().unwrap_or_else(|| "?".to_string()), now_playing = ?np);

        let season = series
            .season(np.season)
            .ok_or_else(|| anyhow!("season not known to Sonarr"))?;

        let is_pilot = np.episode == 1 && np.season == 1;
        let is_only_episode = season
            .statistics
            .as_ref()
            .is_some_and(|s| s.episode_count == 1);
        let is_end_of_season = np.episode
            > season
                .last_episode()
                .unwrap_or(0)
                .saturating_sub(i32::from(self.remaining_episodes));

        if !(is_end_of_season || is_pilot && is_only_episode) {
            debug!(now_playing = ?np, season = ?season, "ignoring early episode");
            return Ok(());
        }

        let next_season = if is_pilot && is_only_episode {
            info!("Stand-alone pilot episode detected, target first season");
            season
        } else if let Some(s) = series.season_mut(np.season + 1) {
            s
        } else {
            info!("Next season not known, monitor new seasons instead");
            series.monitor_new_items = Some(sonarr::NewItemMonitorTypes::All);
            series.monitored = true;
            self.sonarr_client.put_series(&series).await?;
            return Ok(());
        };

        if !self.seen.once(np.clone()) {
            debug!(now_playing = ?np, "skip previously processed item");
            return Ok(());
        }

        let next_season_num = next_season.season_number;

        if let Some(statistics) = &next_season.statistics {
            if statistics.episode_count == statistics.total_episode_count {
                debug!(num = next_season_num, "skip already downloaded season");
                return Ok(());
            }
        }

        info!(num = next_season_num, "Searching next season");

        self.sonarr_client
            .search_season(&series, next_season_num)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use httpmock::Method::{POST, PUT};
    use serde_json::json;
    use tokio::sync::mpsc;

    use crate::{
        media_server::{NowPlaying, Series},
        Message,
    };

    #[tokio::test]
    async fn search_next() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(serde_json::json!(
                    [{
                            "id": 1234,
                            "title": "TestShow",
                            "tvdbId": 5678,
                            "monitored": false,
                            "monitorNewItems": "all",
                            "seasons": [{
                                "seasonNumber": 1,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            },{
                                "seasonNumber": 2,
                                "monitored": false,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 0,
                                    "totalEpisodeCount": 8,
                                }
                            }]
                        }
                    ]
                ));
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(
                        {
                            "id": 1234,
                            "title": "TestShow",
                            "tvdbId": 5678,
                            "monitored": true,
                            "monitorNewItems": "all",
                            "seasons": [{
                                "seasonNumber": 1,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            },{
                                "seasonNumber": 2,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 0,
                                    "totalEpisodeCount": 8,
                                }
                            }]
                        }
                    ));
                then.json_body(json!({}));
            })
            .await;

        let command_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/command")
                    .method(POST)
                    .json_body(json!({
                        "name": "SeasonSearch",
                        "seriesId": 1234,
                        "seasonNumber": 2,
                    }));
                then.json_body(json!({}));
            })
            .await;

        let (tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, crate::once::Seen::default(), 2)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        put_series_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn monitor() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(serde_json::json!(
                    [{
                            "id": 1234,
                            "title": "TestShow",
                            "tvdbId": 5678,
                            "monitored": false,
                            "monitorNewItems": "all",
                            "seasons": [{
                                "seasonNumber": 1,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            }]
                        }
                    ]
                ));
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(
                        {
                            "id": 1234,
                            "title": "TestShow",
                            "tvdbId": 5678,
                            "monitored": true,
                            "monitorNewItems": "all",
                            "seasons": [{
                                "seasonNumber": 1,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            }]
                        }
                    ));
                then.json_body(json!({}));
            })
            .await;

        let (tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, crate::once::Seen::default(), 2)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Tvdb(5678),
            episode: 7,
            season: 1,
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        put_series_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn pilot() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(serde_json::json!(
                    [{
                            "id": 1234,
                            "title": "TestShow",
                            "tvdbId": 5678,
                            "monitored": false,
                            "monitorNewItems": "all",
                            "seasons": [{
                                "seasonNumber": 1,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 1,
                                    "totalEpisodeCount": 8,
                                }
                            }]
                        }
                    ]
                ));
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(
                        {
                            "id": 1234,
                            "title": "TestShow",
                            "tvdbId": 5678,
                            "monitored": true,
                            "monitorNewItems": "all",
                            "seasons": [{
                                "seasonNumber": 1,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 1,
                                    "totalEpisodeCount": 8,
                                }
                            }]
                        }
                    ));
                then.json_body(json!({}));
            })
            .await;

        let command_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/command")
                    .method(POST)
                    .json_body(json!({
                        "name": "SeasonSearch",
                        "seriesId": 1234,
                        "seasonNumber": 1,
                    }));
                then.json_body(json!({}));
            })
            .await;

        let (tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, crate::once::Seen::default(), 2)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 1,
            season: 1,
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        put_series_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }
}
