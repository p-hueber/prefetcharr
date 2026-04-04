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
mod test {
    use serde_json::json;
    use tokio::sync::mpsc;

    use crate::{
        fake_sonarr::{FakeSonarr, make_episode, make_season, make_series},
        media_server::{NowPlaying, Series, test::np_default},
        util::once,
    };

    fn default_series() -> serde_json::Value {
        make_series(
            1234,
            "TestShow",
            5678,
            &[
                make_season(0, false, true),
                make_season(1, false, true),
                make_season(2, false, true),
            ],
        )
    }

    fn default_episodes() -> Vec<serde_json::Value> {
        let mut eps = Vec::new();
        for s in 1..=2 {
            for e in 1..=8 {
                eps.push(make_episode(s * 10 + e, 1234, s, e, false));
            }
        }
        eps
    }

    fn actor(fake: &FakeSonarr, prefetch_num: usize, request_seasons: bool) -> super::Actor {
        let (_tx, rx) = mpsc::channel(1);
        let sonarr = crate::sonarr::Client::new(fake.url(), "secret").unwrap();
        super::Actor::new(
            rx,
            sonarr,
            once::Seen::default(),
            prefetch_num,
            request_seasons,
            None,
        )
    }

    // Prefetching from mid-season triggers season searches for the current and next season
    #[tokio::test]
    #[test_log::test]
    async fn search_next() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeSonarr::start().await;
        fake.add_series(default_series());
        fake.add_episodes(default_episodes());

        actor(&fake, 2, true)
            .prefetch(NowPlaying {
                series: Series::Title("TestShow".to_string()),
                episode: 7,
                season: 1,
                ..np_default()
            })
            .await?;

        assert!(fake.series_state(1234)["monitored"].as_bool().unwrap());
        assert!(
            fake.series_state(1234)["seasons"][1]["monitored"]
                .as_bool()
                .unwrap()
        );
        assert!(
            fake.series_state(1234)["seasons"][2]["monitored"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            fake.commands(),
            vec![
                json!({"name": "SeasonSearch", "seriesId": 1234, "seasonNumber": 1}),
                json!({"name": "SeasonSearch", "seriesId": 1234, "seasonNumber": 2}),
            ]
        );
        Ok(())
    }

    // With request_seasons disabled, prefetches individual episodes across season boundaries
    #[tokio::test]
    #[test_log::test]
    async fn search_episodes() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeSonarr::start().await;
        fake.add_series(default_series());
        fake.add_episodes(default_episodes());

        actor(&fake, 3, false)
            .prefetch(NowPlaying {
                series: Series::Title("TestShow".to_string()),
                episode: 7,
                season: 1,
                ..np_default()
            })
            .await?;

        assert!(fake.series_state(1234)["monitored"].as_bool().unwrap());
        assert!(fake.episode(18)["monitored"].as_bool().unwrap());
        assert!(fake.episode(21)["monitored"].as_bool().unwrap());
        assert!(fake.episode(22)["monitored"].as_bool().unwrap());
        assert!(!fake.episode(11)["monitored"].as_bool().unwrap());
        assert_eq!(
            fake.commands(),
            vec![json!({"name": "EpisodeSearch", "episodeIds": [18, 21, 22]}),]
        );
        Ok(())
    }

    // When fewer episodes remain than prefetch_num, monitors unannounced episodes and searches only what's available
    #[tokio::test]
    #[test_log::test]
    async fn search_episodes_exceeding() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeSonarr::start().await;
        fake.add_series(default_series());
        fake.add_episodes(default_episodes());

        actor(&fake, 3, false)
            .prefetch(NowPlaying {
                series: Series::Title("TestShow".to_string()),
                episode: 7,
                season: 2,
                ..np_default()
            })
            .await?;

        // monitor_unannounced_episodes: series monitored, last season monitored
        assert!(fake.series_state(1234)["monitored"].as_bool().unwrap());
        assert!(
            fake.series_state(1234)["seasons"][2]["monitored"]
                .as_bool()
                .unwrap()
        );
        // Episode monitoring restored: all s2 episodes back to unmonitored except the searched one
        assert!(!fake.episode(21)["monitored"].as_bool().unwrap());
        assert!(fake.episode(28)["monitored"].as_bool().unwrap());
        assert_eq!(
            fake.commands(),
            vec![json!({"name": "EpisodeSearch", "episodeIds": [28]}),]
        );
        Ok(())
    }

    // Watching the pilot triggers unannounced episode monitoring since remaining episodes < prefetch_num
    #[tokio::test]
    #[test_log::test]
    async fn pilot() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeSonarr::start().await;
        fake.add_series(default_series());
        fake.add_episodes(default_episodes());

        actor(&fake, 2, true)
            .prefetch(NowPlaying {
                series: Series::Title("TestShow".to_string()),
                episode: 1,
                season: 1,
                ..np_default()
            })
            .await?;

        // monitor_unannounced_episodes called (only 1 ep remaining in s1 < prefetch_num=2)
        assert!(fake.series_state(1234)["monitored"].as_bool().unwrap());
        assert!(
            fake.series_state(1234)["seasons"][1]["monitored"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            fake.commands(),
            vec![json!({"name": "SeasonSearch", "seriesId": 1234, "seasonNumber": 1}),]
        );
        Ok(())
    }

    // Season 0 (specials) is skipped without triggering any searches or monitoring changes
    #[tokio::test]
    #[test_log::test]
    async fn special_episode() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeSonarr::start().await;
        fake.add_series(default_series());

        actor(&fake, 2, true)
            .prefetch(NowPlaying {
                series: Series::Title("TestShow".to_string()),
                episode: 1,
                season: 0,
                ..np_default()
            })
            .await?;

        assert!(!fake.series_state(1234)["monitored"].as_bool().unwrap());
        assert!(fake.commands().is_empty());
        Ok(())
    }

    // When a season is already monitored, its episodes are explicitly monitored before searching
    #[tokio::test]
    #[test_log::test]
    async fn monitor_episodes_of_monitored_season() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeSonarr::start().await;
        let mut series = default_series();
        series["seasons"][1]["monitored"] = true.into();
        fake.add_series(series);
        fake.add_episodes(default_episodes());

        actor(&fake, 2, true)
            .prefetch(NowPlaying {
                series: Series::Title("TestShow".to_string()),
                episode: 1,
                season: 1,
                ..np_default()
            })
            .await?;

        // Season 1 was already monitored → episodes must be explicitly monitored
        for e in 11..=18 {
            assert!(
                fake.episode(e)["monitored"].as_bool().unwrap(),
                "s1e{} should be monitored",
                e - 10
            );
        }
        assert_eq!(
            fake.commands(),
            vec![json!({"name": "SeasonSearch", "seriesId": 1234, "seasonNumber": 1}),]
        );
        Ok(())
    }

    // Seasons still airing fall back to episode search instead of season search
    #[tokio::test]
    #[test_log::test]
    async fn search_season_not_fully_aired() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeSonarr::start().await;
        let mut series = default_series();
        series["monitored"] = true.into();
        series["seasons"][1] = make_season(1, false, false);
        fake.add_series(series);
        fake.add_episodes(default_episodes());

        actor(&fake, 1, true)
            .prefetch(NowPlaying {
                series: Series::Title("TestShow".to_string()),
                episode: 7,
                season: 1,
                ..np_default()
            })
            .await?;

        assert!(fake.episode(18)["monitored"].as_bool().unwrap());
        assert!(!fake.episode(17)["monitored"].as_bool().unwrap());
        assert_eq!(
            fake.commands(),
            vec![json!({"name": "EpisodeSearch", "episodeIds": [18]}),]
        );
        Ok(())
    }

    // Fully aired seasons use season search while still-airing seasons fall back to episode search
    #[tokio::test]
    #[test_log::test]
    async fn search_season_mixed() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeSonarr::start().await;
        let mut series = default_series();
        series["monitored"] = true.into();
        series["seasons"][1] = make_season(1, false, false);
        fake.add_series(series);
        fake.add_episodes(default_episodes());

        actor(&fake, 2, true)
            .prefetch(NowPlaying {
                series: Series::Title("TestShow".to_string()),
                episode: 7,
                season: 1,
                ..np_default()
            })
            .await?;

        // Season 2 fully aired → SeasonSearch
        assert!(
            fake.series_state(1234)["seasons"][2]["monitored"]
                .as_bool()
                .unwrap()
        );
        // Season 1 still airing → EpisodeSearch for s1e8
        assert!(fake.episode(18)["monitored"].as_bool().unwrap());
        assert_eq!(
            fake.commands(),
            vec![
                json!({"name": "SeasonSearch", "seriesId": 1234, "seasonNumber": 2}),
                json!({"name": "EpisodeSearch", "episodeIds": [18]}),
            ]
        );
        Ok(())
    }
}
