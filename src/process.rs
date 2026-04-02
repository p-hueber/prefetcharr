use std::collections::HashSet;

use anyhow::anyhow;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::{
    Message,
    media_server::{NowPlaying, Series},
    sonarr,
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

        // Season 0 contains specials without any particular order.
        if np.season == 0 {
            return Ok(());
        }

        // fetch n next episodes
        let episodes = self
            .sonarr_client
            .episode_range(&series, np.season, np.episode, self.prefetch_num)
            .await?;

        if episodes.len() < self.prefetch_num {
            info!("Not as many episodes announced, monitor new items instead");
            self.sonarr_client
                .monitor_unannounced_episodes(&mut series)
                .await?;
        } else if !series.monitored {
            series.monitored = true;
            self.sonarr_client.put_series(&series).await?;
        }

        let missing_episodes: Vec<_> = episodes.into_iter().filter(|e| !e.has_file).collect();
        let mut episodes_to_search = Vec::new();

        if self.request_seasons {
            let mut seasons_to_search: HashSet<i32> = HashSet::new();

            for e in missing_episodes {
                if series
                    .season(e.season_number)
                    .is_some_and(sonarr::SeasonResource::is_fully_aired)
                {
                    seasons_to_search.insert(e.season_number);
                } else {
                    episodes_to_search.push(e);
                }
            }

            let mut season_numbers: Vec<_> = seasons_to_search.into_iter().collect();
            season_numbers.sort_unstable();

            let mut error = false;
            for season_num in season_numbers {
                if let Err(err) = self
                    .sonarr_client
                    .search_season(&mut series, season_num)
                    .await
                {
                    error!("skip searching for season {season_num}: {err:#}");
                    error = true;
                }
            }
            if error {
                return Err(anyhow!("failed searching one or more seasons"));
            }
        } else {
            episodes_to_search = missing_episodes;
        }

        if !episodes_to_search.is_empty() {
            let episodes_to_search: Vec<_> = episodes_to_search
                .into_iter()
                .map(|mut e| {
                    e.monitored = true;
                    e
                })
                .collect();
            self.sonarr_client
                .update_episode_monitoring(&episodes_to_search)
                .await?;
            self.sonarr_client
                .search_episodes(&episodes_to_search)
                .await?;
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
                    .query_param("seriesId", "1234")
                    .query_param_count(".*", ".*", 1);
                then.json_body(episodes());
            })
            .await;

        let put_series_mock0 = server
            .mock_async(|when, then| {
                let mut series_monitored = series_unmonitored()[0].clone();
                series_monitored["monitored"] = true.into();
                series_monitored["monitorNewItems"] = "all".into();
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(series_monitored));
                then.json_body(json!({}));
            })
            .await;

        let put_series_mock1 = server
            .mock_async(|when, then| {
                let mut series_monitored = series_unmonitored()[0].clone();
                series_monitored["monitored"] = true.into();
                series_monitored["seasons"][1]["monitored"] = true.into();
                series_monitored["monitorNewItems"] = "all".into();
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(series_monitored));
                then.json_body(json!({}));
            })
            .await;

        let command_mock1 = server
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

        let put_series_mock2 = server
            .mock_async(|when, then| {
                let mut series_monitored = series_unmonitored()[0].clone();
                series_monitored["monitored"] = true.into();
                series_monitored["seasons"][1]["monitored"] = true.into();
                series_monitored["seasons"][2]["monitored"] = true.into();
                series_monitored["monitorNewItems"] = "all".into();
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(series_monitored));
                then.json_body(json!({}));
            })
            .await;

        let command_mock2 = server
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
        episodes_mock.assert_async().await;
        put_series_mock0.assert_async().await;
        put_series_mock1.assert_async().await;
        command_mock1.assert_async().await;
        put_series_mock2.assert_async().await;
        command_mock2.assert_async().await;

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
                    .query_param("seriesId", "1234")
                    .query_param_count(".*", ".*", 1);
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
                    .query_param("seriesId", "1234")
                    .query_param_count(".*", ".*", 1);
                then.json_body(episodes());
            })
            .await;

        let season_episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234")
                    .query_param("seasonNumber", "2")
                    .query_param_count(".*", ".*", 2);
                then.json_body(season_episodes(2));
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

        let put_unmonitor_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(serde_json::json!(
                        {
                          "episodeIds": [ 21, 22, 23, 24, 25, 26, 27, 28 ],
                          "monitored": false
                        }
                    ));
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
        season_episodes_mock.assert_async().await;
        put_series_mock.assert_async().await;
        put_unmonitor_mock.assert_async().await;
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
                        "nextAiring": null,
                    }
                },{
                    "seasonNumber": 1,
                    "monitored": false,
                    "statistics": {
                        "sizeOnDisk": 9000,
                        "episodeCount": 8,
                        "episodeFileCount": 8,
                        "totalEpisodeCount": 8,
                        "nextAiring": null,
                    }
                },{
                    "seasonNumber": 2,
                    "monitored": false,
                    "statistics": {
                        "sizeOnDisk": 9000,
                        "episodeCount": 8,
                        "episodeFileCount": 8,
                        "totalEpisodeCount": 8,
                        "nextAiring": null,
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
                    "nextAiring": null,
                }
            },{
                "seasonNumber": 1,
                "monitored": true,
                "statistics": {
                    "sizeOnDisk": 9000,
                    "episodeCount": 8,
                    "episodeFileCount": 8,
                    "totalEpisodeCount": 8,
                    "nextAiring": null,
                }
            },{
                "seasonNumber": 2,
                "monitored": true,
                "statistics": {
                    "sizeOnDisk": 9000,
                    "episodeCount": 8,
                    "episodeFileCount": 8,
                    "totalEpisodeCount": 8,
                    "nextAiring": null,
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

    fn season_episodes(season: i32) -> serde_json::Value {
        let mut episodes = Vec::new();
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
        episodes.into()
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
                    .query_param("seriesId", "1234")
                    .query_param_count(".*", ".*", 1);
                then.json_body(episodes());
            })
            .await;

        let put_series_mock0 = server
            .mock_async(|when, then| {
                let mut series = series_unmonitored()[0].clone();
                series["monitored"] = serde_json::Value::Bool(true);
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(series);
                then.json_body(json!({}));
            })
            .await;

        let put_series_mock1 = server
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
        episodes_mock.assert_async().await;
        put_series_mock0.assert_async().await;
        put_series_mock1.assert_async().await;
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

        let (_tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        let np = NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 1,
            season: 0,
            ..np_default()
        };
        let handle = tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, true, None)
                .prefetch(np)
                .await
        });

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        handle.await.unwrap().unwrap();

        Ok(())
    }

    #[tokio::test]
    #[test_log::test]
    // If we search for a season and it is already monitored, we must monitor all episodes manually.
    async fn monitor_episodes_of_monitored_season() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                let mut series = series_unmonitored();
                series[0]["seasons"][1]["monitored"] = serde_json::Value::Bool(true);
                when.path("/pathprefix/api/v3/series");
                then.json_body(series);
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234")
                    .query_param_count(".*", ".*", 1);
                then.json_body(episodes());
            })
            .await;

        let season_episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234")
                    .query_param("seasonNumber", "1")
                    .query_param_count(".*", ".*", 2);
                then.json_body(season_episodes(1));
            })
            .await;

        let put_series_mock = server
            .mock_async(|when, then| {
                let mut series = series_unmonitored()[0].clone();
                series["monitored"] = serde_json::Value::Bool(true);
                series["seasons"][1]["monitored"] = serde_json::Value::Bool(true);
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(series);
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
                            11, 12, 13, 14, 15, 16, 17, 18
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
                        "name": "SeasonSearch",
                        "seriesId": 1234,
                        "seasonNumber": 1,
                    }));
                then.json_body(json!({}));
            })
            .await;

        let (_tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        let np = NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 1,
            season: 1,
            ..np_default()
        };
        let handle = tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, true, None)
                .prefetch(np)
                .await
        });

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        episodes_mock.assert_async().await;
        season_episodes_mock.assert_async().await;
        put_series_mock.assert_async().await;
        put_monitor_mock.assert_async().await;
        command_mock.assert_async().await;

        handle.await.unwrap().unwrap();

        Ok(())
    }

    #[tokio::test]
    #[test_log::test]
    // When request_seasons=true but the season is still airing, fall back to individual episode search.
    async fn search_season_not_fully_aired() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let mut series = series_unmonitored();
        series[0]["monitored"] = true.into();
        series[0]["seasons"][1]["statistics"]["nextAiring"] = "2025-06-01T00:00:00Z".into();

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(series.clone());
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234")
                    .query_param_count(".*", ".*", 1);
                then.json_body(episodes());
            })
            .await;

        let put_monitor_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(serde_json::json!({
                        "episodeIds": [18],
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
                        "name": "EpisodeSearch",
                        "episodeIds": [18]
                    }));
                then.json_body(json!({}));
            })
            .await;

        let (_tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        let np = NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
            ..np_default()
        };
        let handle = tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 1, true, None)
                .prefetch(np)
                .await
        });

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        episodes_mock.assert_async().await;
        put_monitor_mock.assert_async().await;
        command_mock.assert_async().await;

        handle.await.unwrap().unwrap();

        Ok(())
    }

    #[tokio::test]
    #[test_log::test]
    // When request_seasons=true and episodes span two seasons — one fully aired, one still airing —
    // the fully aired season gets a SeasonSearch and the airing season gets individual EpisodeSearch.
    async fn search_season_mixed() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let mut series = series_unmonitored();
        series[0]["monitored"] = true.into();
        series[0]["seasons"][1]["statistics"]["nextAiring"] = "2025-06-01T00:00:00Z".into();

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(series.clone());
            })
            .await;

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234")
                    .query_param_count(".*", ".*", 1);
                then.json_body(episodes());
            })
            .await;

        // Season 2 is fully aired and not yet monitored → search_season sets it monitored
        let put_series_mock = server
            .mock_async(|when, then| {
                let mut s = series[0].clone();
                s["seasons"][2]["monitored"] = true.into();
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(s);
                then.json_body(json!({}));
            })
            .await;

        let season_command_mock = server
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

        // Season 1 is still airing → fall back to individual episode search for s1e8 (id=18)
        let put_monitor_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(serde_json::json!({
                        "episodeIds": [18],
                        "monitored": true
                    }));
                then.json_body(json!({}));
            })
            .await;

        let episode_command_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/command")
                    .method(POST)
                    .json_body(json!({
                        "name": "EpisodeSearch",
                        "episodeIds": [18]
                    }));
                then.json_body(json!({}));
            })
            .await;

        let (_tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(&server.url("/pathprefix"), "secret")?;
        let np = NowPlaying {
            series: Series::Title("TestShow".to_string()),
            episode: 7,
            season: 1,
            ..np_default()
        };
        let handle = tokio::spawn(async move {
            super::Actor::new(rx, sonarr, once::Seen::default(), 2, true, None)
                .prefetch(np)
                .await
        });

        tokio::time::sleep(Duration::from_millis(500)).await;

        series_mock.assert_async().await;
        episodes_mock.assert_async().await;
        put_series_mock.assert_async().await;
        season_command_mock.assert_async().await;
        put_monitor_mock.assert_async().await;
        episode_command_mock.assert_async().await;

        handle.await.unwrap().unwrap();

        Ok(())
    }
}
