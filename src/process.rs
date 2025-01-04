use anyhow::anyhow;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::{
    media_server::{NowPlaying, Series},
    sonarr,
    util::once::Seen,
    Message,
};

pub struct Actor {
    rx: mpsc::Receiver<Message>,
    sonarr_client: sonarr::Client,
    seen: Seen,
    remaining_episodes: u8,
    users: Vec<String>,
}

impl Actor {
    pub fn new(
        rx: mpsc::Receiver<Message>,
        sonarr_client: sonarr::Client,
        seen: Seen,
        remaining_episodes: u8,
        users: Vec<String>,
    ) -> Self {
        Self {
            rx,
            sonarr_client,
            seen,
            remaining_episodes,
            users,
        }
    }
}

impl Actor {
    fn is_user_wanted(&self, np: &NowPlaying) -> bool {
        if self.users.is_empty() {
            // Always match if we have no users in the list.
            true
        } else {
            // Match either the user ID or user name.
            self.users.contains(&np.user_id) || self.users.contains(&np.user_name)
        }
    }

    pub async fn process(&mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                Message::NowPlaying(np) => {
                    if !self.is_user_wanted(&np) {
                        debug!(
                            now_playing = ?np,
                            users = ?self.users,
                            "ignoring session from unwanted user"
                        );
                        break;
                    }
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
            .is_some_and(|s| s.episode_file_count == 1);
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

        let next_season_num = next_season.season_number;

        if !self.seen.once(np.series.clone(), next_season_num) {
            debug!(now_playing = ?np, "skip previously processed item");
            return Ok(());
        }

        if let Some(statistics) = &next_season.statistics {
            if statistics.episode_file_count == statistics.total_episode_count
                && statistics.total_episode_count > 0
            {
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
#[allow(clippy::too_many_lines)]
mod test {
    use std::time::Duration;

    use httpmock::Method::{POST, PUT};
    use serde_json::json;
    use tokio::sync::mpsc;

    use crate::{
        media_server::{NowPlaying, Series},
        util::once,
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
                                    "episodeFileCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            },{
                                "seasonNumber": 2,
                                "monitored": false,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 0,
                                    "episodeFileCount": 0,
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
                                    "episodeFileCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            },{
                                "seasonNumber": 2,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 0,
                                    "episodeFileCount": 0,
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
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, vec![])
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
            user_id: "12345".to_string(),
            user_name: "test".to_string(),
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        put_series_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn search_next_filter_users() -> Result<(), Box<dyn std::error::Error>> {
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
                                    "episodeFileCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            },{
                                "seasonNumber": 2,
                                "monitored": false,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 8,
                                    "episodeFileCount": 0,
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
                                    "episodeFileCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            },{
                                "seasonNumber": 2,
                                "monitored": true,
                                "statistics": {
                                    "sizeOnDisk": 9000,
                                    "episodeCount": 8,
                                    "episodeFileCount": 0,
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

        let (tx, rx) = mpsc::channel(3);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(
                rx,
                sonarr,
                once::Seen::default(),
                2,
                vec!["test".to_string(), "12345".to_string()],
            )
            .process()
            .await;
        });

        // Valid user ID
        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
            user_id: "12345".to_string(),
            user_name: "other".to_string(),
        }))
        .await?;
        // Valid username
        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
            user_id: "67890".to_string(),
            user_name: "test".to_string(),
        }))
        .await?;
        // Invalid
        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
            user_id: "67890".to_string(),
            user_name: "unknown".to_string(),
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        // We expect 2 requests to be made for the series search - one for the
        // valid user ID and one for the valid user name.
        series_mock.assert_hits_async(2).await;
        // But we only expect a single request to add the season and run a
        // search.
        put_series_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn search_next_skips_unwanted_users() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;
        let series_mock = server
            .mock_async(|when, _| {
                when.path("/pathprefix/api/v3/series");
            })
            .await;
        let put_series_mock = server
            .mock_async(|when, _| {
                when.path("/pathprefix/api/v3/series/1234").method(PUT);
            })
            .await;
        let command_mock = server
            .mock_async(|when, _| {
                when.path("/pathprefix/api/v3/command").method(POST);
            })
            .await;

        let (tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(
                rx,
                sonarr,
                once::Seen::default(),
                2,
                vec!["test".to_string()],
            )
            .process()
            .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("Some Unknown Show".to_string()),
            episode: 79,
            season: 40,
            user_id: "12345".to_string(),
            user_name: "unwanted".to_string(),
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_hits_async(0).await;
        put_series_mock.assert_hits_async(0).await;
        command_mock.assert_hits_async(0).await;

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
                                    "episodeFileCount": 8,
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
                                    "episodeFileCount": 8,
                                    "totalEpisodeCount": 8,
                                }
                            }]
                        }
                    ));
                then.json_body(json!({}));
            })
            .await;

        let (tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, vec![])
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Tvdb(5678),
            episode: 7,
            season: 1,
            user_id: "12345".to_string(),
            user_name: "test".to_string(),
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
                                    "episodeCount": 8,
                                    "episodeFileCount": 1,
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
                                    "episodeFileCount": 1,
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
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, vec![])
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 1,
            season: 1,
            user_id: "12345".to_string(),
            user_name: "test".to_string(),
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        put_series_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }
}
