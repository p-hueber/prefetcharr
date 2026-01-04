use std::collections::HashSet;

use anyhow::anyhow;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::{
    Message,
    media_server::{NowPlaying, Series},
    sonarr::{self, NewItemMonitorTypes},
    util::once::Seen,
};

pub struct Actor {
    rx: mpsc::Receiver<Message>,
    sonarr_client: sonarr::Client,
    seen: Seen,
    prefetch_num: usize,
    request_seasons: bool,
    exclude_tag: Option<sonarr::Tag>,
}

impl Actor {
    pub fn new(
        rx: mpsc::Receiver<Message>,
        sonarr_client: sonarr::Client,
        seen: Seen,
        prefetch_num: usize,
        request_seasons: bool,
        exclude_tag: Option<String>,
    ) -> Self {
        let exclude_tag = exclude_tag.map(sonarr::Tag::from);
        Self {
            rx,
            sonarr_client,
            seen,
            prefetch_num,
            request_seasons,
            exclude_tag,
        }
    }
}

impl Actor {
    pub async fn process(&mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                Message::NowPlaying(np) => {
                    if let Err(e) = self.prefetch(np).await {
                        error!(err = ?e, "Failed to process");
                    }
                }
            }
        }
    }

    async fn prefetch(&mut self, np: NowPlaying) -> anyhow::Result<()> {
        if !self.seen.once(np.clone()) {
            debug!(now_playing = ?np, "skip previously processed item");
            return Ok(());
        }

        // find series
        let mut series = self.find_series(&np).await?;

        info!(title = series.title.clone().unwrap_or_else(|| "?".to_string()), now_playing = ?np);

        // Resolve and match exclusion tag
        if let Some(exclude_tag) = &mut self.exclude_tag {
            self.sonarr_client.update_tag(exclude_tag).await;
            if let Some(true) = series.is_tagged_with(exclude_tag) {
                info!("excluded via tag");
                return Ok(());
            }
        }

        // fetch n next episodes
        let episodes = self
            .sonarr_client
            .episodes(&series, np.season, np.episode, self.prefetch_num)
            .await?;

        // https://forums.sonarr.tv/t/season-monitor-toggle-option-that-doesnt-change-the-existing-episode-state/30098/9
        // monitor series
        let mut series_modified = !series.monitored;
        series.monitored = true;

        // Not enough episodes announced
        if episodes.len() < self.prefetch_num {
            info!("Not as many episodes announced, monitor new items instead");
            // Maybe more episodes are going to be announced for the last season
            if let Some(last_season) = series.seasons.last_mut() {
                last_season.monitored = true;
            }
            // Maybe more seasons are going to be announced
            series.monitor_new_items = Some(NewItemMonitorTypes::All);
            series_modified = true;
        }

        let missing_episodes = episodes.into_iter().filter(|e| !e.has_file);
        if self.request_seasons {
            // search full seasons
            let mut season_numbers = missing_episodes
                .map(|e| e.season_number)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            season_numbers.sort_unstable();

            series_modified |= series.monitor_seasons(&season_numbers);

            if series_modified {
                self.sonarr_client.put_series(&series).await?;
            }

            for season_num in season_numbers {
                if let Err(err) = self.sonarr_client.search_season(&series, season_num).await {
                    error!("skip searching for season {season_num}: {err:#}");
                }
            }
        } else {
            if series_modified {
                self.sonarr_client.put_series(&series).await?;
            }

            // search single episodes
            let episodes: Vec<_> = missing_episodes.collect();
            self.sonarr_client.monitor_episodes(&episodes).await?;
            self.sonarr_client.search_episodes(&episodes).await?;
        }

        Ok(())
    }

    async fn find_series(
        &mut self,
        np: &NowPlaying,
    ) -> Result<sonarr::SeriesResource, anyhow::Error> {
        let series = self.sonarr_client.series().await?;
        let series = series
            .into_iter()
            .find(|s| match &np.series {
                Series::Title(t) => s.title.as_ref() == Some(t),
                Series::Tvdb(i) => &s.tvdb_id == i,
            })
            .ok_or_else(|| anyhow!("series not found in Sonarr"))?;
        Ok(series)
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
        Message,
        media_server::{NowPlaying, Series, test::np_default},
        util::once,
    };

    #[tokio::test]
    #[test_log::test]
    async fn search_next() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(series_unmonitored());
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234");
                then.json_body(episodes());
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(series_monitored()));
                then.json_body(json!({}));
            })
            .await;

        let monitor_episodes_mock_1 = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(json!({
                        "episodeIds": [11, 12, 13, 14, 15, 16, 17, 18],
                        "monitored": true
                    }));
                then.json_body(json!({}));
            })
            .await;

        let monitor_episodes_mock_2 = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(json!({
                        "episodeIds": [21, 22, 23, 24, 25, 26, 27, 28],
                        "monitored": true
                    }));
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
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, true, None)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
            ..np_default()
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        episodes_mock.assert_hits_async(3).await;
        put_series_mock.assert_async().await;
        monitor_episodes_mock_1.assert_async().await;
        monitor_episodes_mock_2.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    #[test_log::test]
    async fn search_episodes() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(series_unmonitored());
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234");
                then.json_body(episodes());
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                let mut series_monitored = series_unmonitored()[0].clone();
                series_monitored["monitored"] = true.into();
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(series_monitored));
                then.json_body(json!({}));
            })
            .await;

        let put_monitor_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(serde_json::json!(
                        {
                          "episodeIds": [
                            18, 21, 22
                          ],
                          "monitored": true
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
                        "name": "EpisodeSearch",
                        "episodeIds": [ 18, 21, 22 ]
                    }));
                then.json_body(json!({}));
            })
            .await;
        let (tx, rx) = mpsc::channel(1);

        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 3, false, None)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
            ..np_default()
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        episodes_mock.assert_async().await;
        put_series_mock.assert_async().await;
        put_monitor_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    #[test_log::test]
    async fn search_episodes_exceeding() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(series_unmonitored());
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234");
                then.json_body(episodes());
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                let mut series_monitored = series_unmonitored()[0].clone();
                series_monitored["monitored"] = true.into();
                series_monitored["seasons"][2]["monitored"] = true.into();
                series_monitored["monitorNewItems"] = "all".into();
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(series_monitored));
                then.json_body(json!({}));
            })
            .await;

        let put_monitor_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(serde_json::json!(
                        {
                          "episodeIds": [ 28 ],
                          "monitored": true
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
                        "name": "EpisodeSearch",
                        "episodeIds": [ 28 ]
                    }));
                then.json_body(json!({}));
            })
            .await;
        let (tx, rx) = mpsc::channel(1);

        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 3, false, None)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 2,
            ..np_default()
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        episodes_mock.assert_async().await;
        put_series_mock.assert_async().await;
        put_monitor_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    fn series_unmonitored() -> serde_json::Value {
        serde_json::json!(
            [{
                "id": 1234,
                "title": "TestShow",
                "tvdbId": 5678,
                "monitored": false,
                "monitorNewItems": "all",
                "seasons": [{
                    "seasonNumber": 0,
                    "monitored": false,
                    "statistics": {
                        "sizeOnDisk": 9000,
                        "episodeCount": 8,
                        "episodeFileCount": 8,
                        "totalEpisodeCount": 8,
                    }
                },{
                    "seasonNumber": 1,
                    "monitored": false,
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
                        "episodeFileCount": 8,
                        "totalEpisodeCount": 8,
                    }
                }]
            }]
        )
    }

    fn series_monitored() -> serde_json::Value {
        serde_json::json!({
            "id": 1234,
            "title": "TestShow",
            "tvdbId": 5678,
            "monitored": true,
            "monitorNewItems": "all",
            "seasons": [{
                "seasonNumber": 0,
                "monitored": false,
                "statistics": {
                    "sizeOnDisk": 9000,
                    "episodeCount": 8,
                    "episodeFileCount": 8,
                    "totalEpisodeCount": 8,
                }
            },{
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
                    "episodeFileCount": 8,
                    "totalEpisodeCount": 8,
                }
                }]
        })
    }

    fn episodes() -> serde_json::Value {
        let mut episodes = Vec::new();
        for season in 1..3 {
            for episode in 1..9 {
                let episode = serde_json::json!({
                    "id": season*10 + episode,
                    "seasonNumber": season,
                    "episodeNumber": episode,
                    "hasFile": false,
                    "monitored": false,
                });
                episodes.push(episode);
            }
        }
        episodes.into()
    }

    #[tokio::test]
    #[test_log::test]
    async fn monitor() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(series_unmonitored());
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234");
                then.json_body(episodes());
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(series_monitored());
                then.json_body(json!({}));
            })
            .await;

        let monitor_episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(json!({
                        "episodeIds": [11, 12, 13, 14, 15, 16, 17, 18],
                        "monitored": true
                    }));
                then.json_body(json!({}));
            })
            .await;

        let (tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, true, None)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Tvdb(5678),
            episode: 7,
            season: 1,
            ..np_default()
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        episodes_mock.assert_hits_async(3).await;
        put_series_mock.assert_async().await;
        monitor_episodes_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    #[test_log::test]
    async fn pilot() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(series_unmonitored());
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234");
                then.json_body(episodes());
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                let mut series_monitored = series_monitored();
                series_monitored["seasons"][0]["monitored"] = serde_json::Value::Bool(false);
                series_monitored["seasons"][2]["monitored"] = serde_json::Value::Bool(false);
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(series_monitored);
                then.json_body(json!({}));
            })
            .await;

        let monitor_episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(json!({
                        "episodeIds": [11, 12, 13, 14, 15, 16, 17, 18],
                        "monitored": true
                    }));
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
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, true, None)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 1,
            season: 1,
            ..np_default()
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        episodes_mock.assert_hits_async(2).await;
        put_series_mock.assert_async().await;
        monitor_episodes_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    #[test_log::test]
    async fn special_episode() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(series_unmonitored());
            })
            .await;

        let (tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, true, None)
                .process()
                .await;
        });

        tx.send(Message::NowPlaying(NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 1,
            season: 0,
            ..np_default()
        }))
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;

        Ok(())
    }
}
