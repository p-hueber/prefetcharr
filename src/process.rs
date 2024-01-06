use anyhow::anyhow;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::{jellyfin::NowPlaying, once::Seen, sonarr, Message};

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
            .find(|s| s.tvdb_id == np.tvdb)
            .ok_or_else(|| anyhow!("series not found in Sonarr"))?;

        info!(title = series.title.clone().unwrap_or_else(|| "?".to_string()), now_playing = ?np);

        let season = series
            .season(np.season)
            .ok_or_else(|| anyhow!("season not known to Sonarr"))?;

        if np.episode
            <= season
                .last_episode()
                .saturating_sub(i32::from(self.remaining_episodes))
        {
            debug!(now_playing = ?np, season = ?season, "ignoring early episode");
            return Ok(());
        }

        let Some(next_season) = series.season_mut(np.season + 1) else {
            info!("Next season not known, monitor new seasons instead");
            series.monitor_new_items = Some(sonarr::NewItemMonitorTypes::All);
            series.monitored = true;
            self.sonarr_client.put_series(&series).await?;
            return Ok(());
        };

        if !self.seen.once(&np) {
            debug!(now_playing = ?np, "skip previously processed item");
            return Ok(());
        }

        let next_season_num = next_season.season_number;

        if next_season.statistics.episode_count == next_season.statistics.total_episode_count {
            debug!(num = next_season_num, "skip already downloaded season");
            return Ok(());
        }

        info!(num = next_season_num, "Searching next season");

        self.sonarr_client
            .search_season(&series, next_season_num)
            .await?;

        Ok(())
    }
}
