use anyhow::anyhow;
use tracing::{debug, info};

use crate::{jellyfin::NowPlaying, once::Seen, sonarr};

pub async fn search_next(
    np: NowPlaying,
    sonarr_client: &sonarr::Client,
    seen: &mut Seen,
) -> anyhow::Result<()> {
    let series = sonarr_client.series().await?;
    let mut series = series
        .into_iter()
        .find(|s| s.tvdb_id == np.tvdb)
        .ok_or_else(|| anyhow!("series not found in Sonarr"))?;

    info!(title = series.title.clone().unwrap_or_else(|| "?".to_string()), now_playing = ?np);

    let season = series
        .season(np.season)
        .ok_or_else(|| anyhow!("season not known to Sonarr"))?;

    // Fetch from the second-to-last episode so the next season is available when the
    // last episode plays. This should allow Jellyfin to display "next episode" in time.
    if np.episode < season.last_episode() - 1 {
        debug!(now_playing = ?np, season = ?season, "ignoring early episode");
        return Ok(());
    }

    let next_season = series
        .season_mut(np.season + 1)
        .ok_or_else(|| anyhow!("season not known to Sonarr"))?;
    if next_season.monitored {
        debug!(next_season = ?next_season, "season is monitored already");
        return Ok(());
    }

    if !seen.once(&np) {
        debug!(now_playing = ?np, "skip previously processed item");
        return Ok(());
    }

    let next_season_num = next_season.season_number;
    info!(num = next_season_num, "Searching next season");

    sonarr_client
        .search_season(&series, next_season_num)
        .await?;

    Ok(())
}
