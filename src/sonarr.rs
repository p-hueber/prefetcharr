use anyhow::{Context, Result, anyhow};
use reqwest::{
    Url,
    header::{HeaderMap, HeaderValue},
};
use rustls_platform_verifier::ConfigVerifierExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracing::{debug, error, info, instrument, warn};

#[derive(Debug)]
pub enum Tag {
    Label(String),
    Id(i32),
}

#[derive(Clone)]
pub struct Client {
    base_url: Url,
    client: reqwest::Client,
}

impl Client {
    pub fn new(base_url: &str, api_key: &str) -> Result<Self> {
        let mut api_key = HeaderValue::from_str(api_key)?;
        api_key.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("X-Api-Key", api_key);
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .tls_backend_preconfigured(rustls::ClientConfig::with_platform_verifier()?)
            .build()?;

        let base_url = base_url.parse()?;

        Ok(Self { base_url, client })
    }

    async fn get<Out: DeserializeOwned, Param: Serialize + ?Sized>(
        &self,
        path: &str,
        params: Option<&Param>,
    ) -> Result<Out> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .push("api")
            .push("v3")
            .extend(path.split('/'));
        let get = self.client.get(url);
        let get = if let Some(params) = params {
            get.query(params)
        } else {
            get
        };
        let response = get.send().await?.error_for_status()?;
        Ok(response.json::<Out>().await?)
    }

    pub async fn probe(&self) -> Result<()> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .push("api");
        self.client.get(url).send().await?.error_for_status()?;
        Ok(())
    }

    pub async fn put_series(&self, series: &SeriesResource) -> Result<serde_json::Value> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .push("api")
            .push("v3")
            .push("series")
            .push(&series.id.to_string());
        let response = self
            .client
            .put(url)
            .json(series)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }

    #[instrument(skip_all)]
    pub async fn series(&self) -> Result<Vec<SeriesResource>> {
        let series = self
            .get::<Value, ()>("series", None)
            .await?
            .as_array()
            .context("not an array")?
            .iter()
            .filter_map(|s| {
                serde_json::from_value(s.clone())
                    .inspect_err(|e| debug!(series=?s, "ignoring malformed series entry: {e}"))
                    .ok()
            })
            .collect::<Vec<SeriesResource>>();
        Ok(series)
    }

    #[instrument(skip_all)]
    async fn tags(&self) -> Result<Vec<TagResource>> {
        let tags = self
            .get::<Value, ()>("tag", None)
            .await?
            .as_array()
            .context("not an array")?
            .iter()
            .filter_map(|t| {
                serde_json::from_value(t.clone())
                    .inspect_err(|e| debug!(tags=?t, "ignoring malformed tags entry: {e}"))
                    .ok()
            })
            .collect::<Vec<_>>();
        Ok(tags)
    }

    #[instrument(skip(self))]
    pub async fn resolve_tag(&self, label: &str) -> Result<i32> {
        self.tags()
            .await
            .context("retrieving tags")?
            .into_iter()
            .find_map(|t| (t.label.as_deref() == Some(label)).then_some(t.id))
            .context("tag not known")
    }

    #[instrument(skip(self))]
    pub async fn update_tag(&self, tag: &mut Tag) {
        let Tag::Label(label) = tag else { return };
        match self.resolve_tag(label).await {
            Ok(id) => *tag = Tag::Id(id),
            Err(err) => {
                // Not a hard error as the tag may be added to Sonarr later.
                warn!(tag=%label, "cannot resolve tag ID: {err:#}");
            }
        }
    }

    async fn set_monitored_episodes(
        &self,
        episode_ids: Vec<i32>,
        monitored: bool,
    ) -> Result<serde_json::Value> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .push("api")
            .push("v3")
            .push("episode")
            .push("monitor");

        let request = EpisodeMonitoredResource {
            episode_ids,
            monitored,
        };

        let response = self
            .client
            .put(url)
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        Ok(response.json().await?)
    }

    pub async fn update_episode_monitoring(&self, episodes: &[EpisodeResource]) -> Result<()> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .push("api")
            .push("v3")
            .push("episode")
            .push("monitor");

        let monitored_ids: Vec<_> = episodes
            .iter()
            .filter_map(|e| e.monitored.then_some(e.id))
            .collect();

        let unmonitored_ids: Vec<_> = episodes
            .iter()
            .filter_map(|e| (!e.monitored).then_some(e.id))
            .collect();

        if !monitored_ids.is_empty() {
            self.set_monitored_episodes(monitored_ids, true).await?;
        }

        if !unmonitored_ids.is_empty() {
            self.set_monitored_episodes(unmonitored_ids, false).await?;
        }

        Ok(())
    }

    async fn episodes(&self, series: &SeriesResource) -> Result<Vec<EpisodeResource>> {
        self.get("episode", Some(&[("seriesId", series.id)]))
            .await
            .context("error fetching episodes")
    }

    async fn episodes_season(
        &self,
        series: &SeriesResource,
        season: &SeasonResource,
    ) -> Result<Vec<EpisodeResource>> {
        self.get(
            "episode",
            Some(&[
                ("seriesId", series.id),
                ("seasonNumber", season.season_number),
            ]),
        )
        .await
        .context("error fetching episodes")
    }

    pub async fn episode_range(
        &self,
        series: &SeriesResource,
        season_start: i32,
        episode_start: i32,
        num: usize,
    ) -> Result<Vec<EpisodeResource>> {
        let episodes = self.episodes(series).await?;
        let episodes = episode_window(season_start, episode_start, num, episodes);

        Ok(episodes)
    }

    // Make sure all newly announced episodes will be monitored.
    // https://forums.sonarr.tv/t/season-monitor-toggle-option-that-doesnt-change-the-existing-episode-state/30098/9
    pub async fn monitor_unannounced_episodes(&self, series: &mut SeriesResource, check_aired: bool) -> Result<()> {
        // Make series eligible for monitoring checks
        series.monitored = true;

        // Monitor new seasons
        series.monitor_new_items = Some(NewItemMonitorTypes::All);

        // Monitor new episode announcements in last season
        if let Some(last_season) = series.seasons.last_mut() {
            last_season.monitored = true;
        }

        if let Some(last_season) = series.seasons.last() {
            // Apply monitoring but restore episode state
            let mut original_episodes = self.episodes_season(series, last_season).await?;
            // When check_aired is enabled, only monitor episodes that can be prefetched
            // When disabled, monitor all episodes (original behavior)
            if check_aired {
                for e in &mut original_episodes {
                    // Only monitor episodes that can be prefetched (have valid past air dates)
                    e.monitored = e.can_prefetch(true);
                }
            } else {
                for e in &mut original_episodes {
                    e.monitored = true;
                }
            }
            self.put_series(series).await?;
            self.update_episode_monitoring(&original_episodes).await?;
        } else {
            // Apply monitoring
            self.put_series(series).await?;
        }

        Ok(())
    }

    pub async fn search_episodes(&self, episodes: &[EpisodeResource]) -> Result<serde_json::Value> {
        let episode_ids: Vec<_> = episodes.iter().map(|e| e.id).collect();
        info!(?episode_ids, "Searching episodes");
        let cmd = json!({
            "name": "EpisodeSearch",
            "episodeIds": episode_ids,
        });

        self.command(cmd).await
    }

    pub async fn search_season(
        &self,
        series: &mut SeriesResource,
        season_num: i32,
        check_aired: bool,
    ) -> Result<serde_json::Value> {
        info!(num = season_num, "Searching season");

        let season = series
            .season(season_num)
            .with_context(|| format!("there is no season {season_num}"))?;

        if season.monitored {
            let mut season_episodes = self.episodes_season(series, season).await?;
            for e in &mut season_episodes {
                if !check_aired || e.can_prefetch(true) {
                    e.monitored = true;
                }
            }
            self.update_episode_monitoring(&season_episodes).await?;
        }

        if !season.monitored {
            let season = series
                .season_mut(season_num)
                .with_context(|| format!("there is no season {season_num}"))?;
            season.monitored = true;
            self.put_series(series).await?;
        }

        let cmd = json!({
            "name": "SeasonSearch",
            "seriesId": series.id,
            "seasonNumber": season_num,
        });

        self.command(cmd).await
    }

    async fn command(&self, cmd: Value) -> std::result::Result<Value, anyhow::Error> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .push("api")
            .push("v3")
            .push("command");

        let response = self
            .client
            .post(url)
            .json(&cmd)
            .send()
            .await?
            .error_for_status()?;

        Ok(response.json().await?)
    }
}

impl From<String> for Tag {
    fn from(label: String) -> Self {
        Tag::Label(label)
    }
}

fn episode_window(
    season_start: i32,
    episode_start: i32,
    num: usize,
    mut episodes: Vec<EpisodeResource>,
) -> Vec<EpisodeResource> {
    episodes.sort_by_key(|e| (e.season_number, e.episode_number));

    episodes
        .into_iter()
        .skip_while(|ep| ep.season_number != season_start || ep.episode_number != episode_start)
        .skip(1)
        .scan((season_start, episode_start), |(prev_s, prev_ep), ep| {
            let season_delta = ep.season_number.saturating_sub(*prev_s);
            let episode_delta = ep.episode_number.saturating_sub(*prev_ep);
            *prev_s = ep.season_number;
            *prev_ep = ep.episode_number;

            // filter gaps
            if (season_delta == 1 && ep.episode_number == 1)
                || (season_delta == 0 && episode_delta == 1)
            {
                Some(ep)
            } else if season_delta == 0 && episode_delta == 0 {
                warn!(?ep, "duplicated episode listing");
                Some(ep)
            } else {
                error!(?ep, "gap in the episode listing");
                None
            }
        })
        .take(num)
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeResource {
    pub id: i32,
    pub season_number: i32,
    pub episode_number: i32,
    pub has_file: bool,
    pub monitored: bool,
    #[serde(flatten)]
    other: serde_json::Value,
}

impl EpisodeResource {
    /// Get the air date string, trying airDateUtc first (RFC3339 format), then airDate
    fn get_air_date_str(&self) -> Option<&str> {
        // Try airDateUtc first as it's in proper RFC3339 format
        self.other.get("airDateUtc")
            .and_then(|v| v.as_str())
            .or_else(|| {
                // Fall back to airDate
                self.other.get("airDate")
                    .and_then(|v| v.as_str())
            })
    }

    /// Check if the episode has aired based on its airDate field
    pub fn has_aired(&self) -> bool {
        let air_date_str = self.get_air_date_str();
        
        match air_date_str {
            None => true, // No air date info, assume aired
            Some(date_str) => {
                // Parse ISO 8601/RFC3339 date string
                if let Ok(air_date) = time::OffsetDateTime::parse(date_str, &time::format_description::well_known::Rfc3339) {
                    let now = time::OffsetDateTime::now_utc();
                    air_date <= now
                } else {
                    // If parsing fails, assume it has aired
                    true
                }
            }
        }
    }
    
    /// Check if the episode can be prefetched based on air date and check_aired setting
    /// When check_aired is true, only prefetch episodes with valid past/future air dates
    /// that have already aired. Episodes without air dates are excluded.
    /// When check_aired is false, all episodes can be prefetched (original behavior).
    pub fn can_prefetch(&self, check_aired: bool) -> bool {
        if !check_aired {
            return true;
        }
        
        // When check_aired is true, only prefetch episodes with valid air dates that have aired
        let air_date_str = self.get_air_date_str();
        
        match air_date_str {
            None => false, // No air date info, don't prefetch when check_aired is true
            Some(date_str) => {
                // Parse ISO 8601/RFC3339 date string
                if let Ok(air_date) = time::OffsetDateTime::parse(date_str, &time::format_description::well_known::Rfc3339) {
                    let now = time::OffsetDateTime::now_utc();
                    air_date <= now
                } else {
                    // If parsing fails, don't prefetch (we can't verify the air date)
                    false
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonStatisticsResource {
    pub size_on_disk: i64,
    pub episode_count: i32,
    pub episode_file_count: i32,
    pub total_episode_count: i32,
    pub next_airing: Option<String>,
    #[serde(flatten)]
    other: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonResource {
    pub season_number: i32,
    pub monitored: bool,
    pub statistics: Option<SeasonStatisticsResource>,
    #[serde(flatten)]
    other: serde_json::Value,
}

impl SeasonResource {
    pub fn is_fully_aired(&self) -> bool {
        !matches!(
            self.statistics,
            Some(SeasonStatisticsResource {
                next_airing: Some(_),
                ..
            })
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NewItemMonitorTypes {
    All,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesResource {
    pub id: i32,
    pub title: Option<String>,
    pub tvdb_id: i32,
    pub monitored: bool,
    // optional for v3 compatibility
    pub monitor_new_items: Option<NewItemMonitorTypes>,
    pub seasons: Vec<SeasonResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<i32>>,
    #[serde(flatten)]
    other: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagResource {
    pub id: i32,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeMonitoredResource {
    pub episode_ids: Vec<i32>,
    pub monitored: bool,
}

impl SeriesResource {
    pub fn season_mut(&mut self, num: i32) -> Option<&mut SeasonResource> {
        self.seasons.iter_mut().find(|s| s.season_number == num)
    }

    pub fn season(&self, num: i32) -> Option<&SeasonResource> {
        self.seasons.iter().find(|s| s.season_number == num)
    }

    pub fn is_tagged_with(&self, tag: &Tag) -> Option<bool> {
        let Tag::Id(id) = tag else { return None };
        Some(self.tags.as_ref()?.contains(id))
    }
}

#[cfg(test)]
mod test {
    use httpmock::Method::{GET, POST, PUT};
    use serde_json::{Value, json};

    use crate::sonarr::{
        EpisodeResource, NewItemMonitorTypes, SeasonResource, SeasonStatisticsResource,
        SeriesResource, Tag,
    };

    // API key is sent via X-Api-Key header on requests
    #[tokio::test]
    async fn auth() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series")
                    .header("X-Api-Key", "secret");
                then.json_body(serde_json::json!([]));
            })
            .await;
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        let _ = client.series().await?;

        series_mock.assert_async().await;

        Ok(())
    }

    // Parses series response without monitorNewItems field (Sonarr v3 compatibility)
    #[tokio::test]
    async fn series_v3() -> Result<(), Box<dyn std::error::Error>> {
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
                        "seasons": []
                    }]
                ));
            })
            .await;
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        let series = client.series().await?;
        assert_eq!(series[0].id, 1234);

        series_mock.assert_async().await;

        Ok(())
    }

    // Parses multiple series entries from a single API response
    #[tokio::test]
    async fn series_multiple() -> Result<(), Box<dyn std::error::Error>> {
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
                        "seasons": []
                    },{
                        "id": 1234,
                        "title": "TestShow",
                        "tvdbId": 5678,
                        "monitored": false,
                        "monitorNewItems": "all",
                        "seasons": []
                    }]
                ));
            })
            .await;
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        let series = client.series().await?;
        assert_eq!(series.len(), 2);

        series_mock.assert_async().await;

        Ok(())
    }

    // Parses seasons that lack a statistics field
    #[tokio::test]
    async fn series_parse_missing_statistics() -> Result<(), Box<dyn std::error::Error>> {
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
                            "seasonNumber": 0,
                            "monitored": false
                        }]
                    }]
                ));
            })
            .await;
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        let series = client.series().await?;
        assert_eq!(series.len(), 1);

        series_mock.assert_async().await;

        Ok(())
    }

    // Malformed series entries are silently skipped, valid ones still returned
    #[tokio::test]
    async fn series_skip_malformed_series() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(serde_json::json!(
                    [{
                        "invalid": "TestShow",
                    },{
                        "id": 1234,
                        "title": "TestShow",
                        "tvdbId": 5678,
                        "monitored": false,
                        "monitorNewItems": "all",
                        "seasons": []
                    }]
                ));
            })
            .await;
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        let series = client.series().await?;
        assert_eq!(series.len(), 1);

        series_mock.assert_async().await;

        Ok(())
    }

    // Empty series list from the API returns an empty vec
    #[tokio::test]
    async fn series_emtpy() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series");
                then.json_body(serde_json::json!([]));
            })
            .await;
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        let series = client.series().await?;
        assert_eq!(series.len(), 0);

        series_mock.assert_async().await;

        Ok(())
    }

    // PUT request serializes series resource with correct camelCase JSON body
    #[tokio::test]
    async fn put_series() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series = SeriesResource {
            id: 1234,
            title: Some("TestShow".to_string()),
            tvdb_id: 5678,
            monitored: false,
            monitor_new_items: Some(NewItemMonitorTypes::All),
            seasons: vec![],
            tags: Some(vec![1]),
            other: Value::Null,
        };

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!(
                        {
                            "id": 1234,
                            "title": "TestShow",
                            "tvdbId": 5678,
                            "monitored": false,
                            "monitorNewItems": "all",
                            "seasons": [],
                            "tags": [1]
                        }
                    ));
                then.json_body(json!({}));
            })
            .await;
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        client.put_series(&series).await?;

        series_mock.assert_async().await;

        Ok(())
    }

    // Season search monitors the season via PUT, then issues a SeasonSearch command
    #[tokio::test]
    async fn search_season() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let season = SeasonResource {
            season_number: 1,
            monitored: false,
            statistics: SeasonStatisticsResource {
                size_on_disk: 9000,
                episode_count: 8,
                episode_file_count: 8,
                total_episode_count: 0,
                next_airing: None,
                other: Value::Null,
            }
            .into(),
            other: Value::Null,
        };

        let mut series = SeriesResource {
            id: 1234,
            title: Some("TestShow".to_string()),
            tvdb_id: 5678,
            monitored: false,
            monitor_new_items: Some(NewItemMonitorTypes::All),
            seasons: vec![season],
            tags: None,
            other: serde_json::json!({}),
        };

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

        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .method(PUT)
                    .json_body(serde_json::json!({
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
                                "totalEpisodeCount": 0,
                                "nextAiring": null,
                            }
                        }]
                    }));
                then.json_body(json!({}));
            })
            .await;

        client.search_season(&mut series, 1, false).await?;

        series_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    // Already-monitored season explicitly monitors all episodes before searching
    #[tokio::test]
    async fn search_season_already_monitored() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let season = SeasonResource {
            season_number: 1,
            monitored: true,
            statistics: SeasonStatisticsResource {
                size_on_disk: 9000,
                episode_count: 2,
                episode_file_count: 0,
                total_episode_count: 2,
                next_airing: None,
                other: Value::Null,
            }
            .into(),
            other: Value::Null,
        };

        let mut series = SeriesResource {
            id: 1234,
            title: Some("TestShow".to_string()),
            tvdb_id: 5678,
            monitored: false,
            monitor_new_items: Some(NewItemMonitorTypes::All),
            seasons: vec![season],
            tags: None,
            other: serde_json::json!({}),
        };

        let episodes_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode")
                    .query_param("seriesId", "1234")
                    .query_param("seasonNumber", "1")
                    .method(GET);
                then.json_body(json!([
                    {"id": 1, "seasonNumber": 1, "episodeNumber": 1, "hasFile": false, "monitored": false},
                    {"id": 2, "seasonNumber": 1, "episodeNumber": 2, "hasFile": false, "monitored": false},
                ]));
            })
            .await;

        let monitor_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/episode/monitor")
                    .method(PUT)
                    .json_body(json!({"episodeIds": [1, 2], "monitored": true}));
                then.json_body(json!([]));
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

        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        client.search_season(&mut series, 1, false).await?;

        episodes_mock.assert_async().await;
        monitor_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    // Returns empty when requesting 0 episodes or when no next episode exists
    #[test]
    fn episode_window_none() {
        let episodes = vec![EpisodeResource {
            ..default_episode()
        }];

        assert!(super::episode_window(1, 1, 0, episodes.clone()).is_empty());
        assert!(super::episode_window(1, 1, 1, episodes.clone()).is_empty());
    }

    // Stops collecting episodes when there is a gap in episode or season numbering
    #[test]
    fn episode_window_gap() {
        let episodes = vec![
            EpisodeResource {
                episode_number: 1,
                ..default_episode()
            },
            EpisodeResource {
                episode_number: 3,
                ..default_episode()
            },
        ];

        assert!(super::episode_window(1, 1, 1, episodes.clone()).is_empty());

        let episodes = vec![
            EpisodeResource {
                season_number: 1,
                ..default_episode()
            },
            EpisodeResource {
                season_number: 3,
                ..default_episode()
            },
        ];

        assert!(super::episode_window(1, 1, 1, episodes.clone()).is_empty());

        let episodes = vec![
            EpisodeResource {
                episode_number: 1,
                season_number: 1,
                ..default_episode()
            },
            EpisodeResource {
                episode_number: 2,
                season_number: 2,
                ..default_episode()
            },
        ];

        assert!(super::episode_window(1, 1, 1, episodes.clone()).is_empty());
    }

    // Window continues into the next season when the current season ends
    #[test]
    fn episode_window_next_season() {
        let episodes = vec![
            EpisodeResource {
                episode_number: 8,
                season_number: 1,
                ..default_episode()
            },
            EpisodeResource {
                episode_number: 1,
                season_number: 2,
                ..default_episode()
            },
        ];

        let res = super::episode_window(1, 8, 1, episodes.clone());
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].season_number, 2);
    }

    // Returns multiple consecutive episodes spanning a season boundary
    #[test]
    fn episode_window_several() {
        let episodes = vec![
            EpisodeResource {
                episode_number: 8,
                season_number: 1,
                ..default_episode()
            },
            EpisodeResource {
                episode_number: 1,
                season_number: 2,
                ..default_episode()
            },
            EpisodeResource {
                episode_number: 2,
                season_number: 2,
                ..default_episode()
            },
        ];

        let res = super::episode_window(1, 8, 2, episodes.clone());
        assert_eq!(res.len(), 2);

        assert_eq!(res[0].episode_number, 1);
        assert_eq!(res[0].season_number, 2);

        assert_eq!(res[1].episode_number, 2);
        assert_eq!(res[1].season_number, 2);
    }

    // Duplicate episode listings are included with a warning instead of breaking the window
    #[test]
    fn episode_window_duplicate() {
        let episodes = vec![
            EpisodeResource {
                episode_number: 1,
                ..default_episode()
            },
            EpisodeResource {
                id: 2,
                episode_number: 2,
                ..default_episode()
            },
            EpisodeResource {
                id: 3,
                episode_number: 2,
                ..default_episode()
            },
            EpisodeResource {
                id: 4,
                episode_number: 3,
                ..default_episode()
            },
        ];

        let res = super::episode_window(1, 1, 3, episodes);
        assert_eq!(res.len(), 3);
        assert_eq!(res[0].episode_number, 2);
        assert_eq!(res[1].episode_number, 2);
        assert_eq!(res[2].episode_number, 3);
    }

    // Matches series by resolved tag ID; returns None for unresolved label
    #[test]
    fn series_match_tag() {
        let series: SeriesResource = serde_json::from_value(serde_json::json!(
            {
                "id": 1234,
                "title": "TestShow",
                "tvdbId": 5678,
                "monitored": false,
                "monitorNewItems": "all",
                "seasons": [],
                "tags": [1, 2]
            }
        ))
        .unwrap();
        assert!(series.is_tagged_with(&crate::sonarr::Tag::Id(1)).unwrap());
        assert!(series.is_tagged_with(&crate::sonarr::Tag::Id(2)).unwrap());
        assert!(!series.is_tagged_with(&crate::sonarr::Tag::Id(3)).unwrap());
        assert!(
            series
                .is_tagged_with(&crate::sonarr::Tag::Label(String::from("1")))
                .is_none()
        );
    }

    // Returns None when the series has no tags field at all
    #[test]
    fn series_no_tag() {
        let series: SeriesResource = serde_json::from_value(serde_json::json!(
            {
                "id": 1234,
                "title": "TestShow",
                "tvdbId": 5678,
                "monitored": false,
                "monitorNewItems": "all",
                "seasons": []
            }
        ))
        .unwrap();
        assert!(series.is_tagged_with(&crate::sonarr::Tag::Id(1)).is_none());
        assert!(
            series
                .is_tagged_with(&crate::sonarr::Tag::Label(String::from("1")))
                .is_none()
        );
    }

    // Resolves label to tag ID, leaves unknown labels unresolved, and skips already-resolved IDs
    #[tokio::test]
    async fn update_tag() -> anyhow::Result<()> {
        let server = httpmock::MockServer::start_async().await;
        let tags_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/tag").method(GET);
                then.json_body(json!(
                    [ { "id": 1, "label": "tag1" } ]
                ));
            })
            .await;

        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        {
            let mut tag = Tag::from(String::from("tag1"));
            client.update_tag(&mut tag).await;
            assert!(matches!(tag, Tag::Id(1)));
        }

        {
            let mut tag = Tag::from(String::from("tag2"));
            client.update_tag(&mut tag).await;
            assert!(matches!(tag, Tag::Label(_)));
        }

        {
            let mut tag = Tag::Id(1);
            client.update_tag(&mut tag).await;
            assert!(matches!(tag, Tag::Id(1)));
        }

        tags_mock.assert_calls_async(2).await;

        Ok(())
    }

    // Tag entry without a label in Sonarr leaves the local label unresolved
    #[tokio::test]
    async fn update_tag_no_label() -> anyhow::Result<()> {
        let server = httpmock::MockServer::start_async().await;
        let tags_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/tag").method(GET);
                then.json_body(json!(
                    [ { "id": 1 } ]
                ));
            })
            .await;

        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        {
            let mut tag = Tag::from(String::from("tag1"));
            client.update_tag(&mut tag).await;
            assert!(matches!(tag, Tag::Label(_)));
        }

        tags_mock.assert_async().await;

        Ok(())
    }

    // Empty tag list from Sonarr leaves the label unresolved
    #[tokio::test]
    async fn update_tag_no_tags() -> anyhow::Result<()> {
        let server = httpmock::MockServer::start_async().await;
        let tags_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/tag").method(GET);
                then.json_body(json!([]));
            })
            .await;

        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        {
            let mut tag = Tag::from(String::from("tag1"));
            client.update_tag(&mut tag).await;
            assert!(matches!(tag, Tag::Label(_)));
        }

        tags_mock.assert_async().await;

        Ok(())
    }

    fn default_episode() -> EpisodeResource {
        EpisodeResource {
            id: 1,
            season_number: 1,
            episode_number: 1,
            has_file: false,
            monitored: false,
            other: Value::default(),
        }
    }

    // Season with next_airing = None is considered fully aired
    #[test]
    fn fully_aired_with_next_airing_null() {
        let season = SeasonResource {
            season_number: 1,
            monitored: true,
            statistics: Some(SeasonStatisticsResource {
                size_on_disk: 1000,
                episode_count: 8,
                episode_file_count: 8,
                total_episode_count: 8,
                next_airing: None,
                other: Value::Null,
            }),
            other: Value::Null,
        };
        assert!(season.is_fully_aired());
    }

    // Season with a future next_airing date is not fully aired
    #[test]
    fn not_fully_aired_with_next_airing() {
        let season = SeasonResource {
            season_number: 1,
            monitored: true,
            statistics: Some(SeasonStatisticsResource {
                size_on_disk: 1000,
                episode_count: 8,
                episode_file_count: 8,
                total_episode_count: 8,
                next_airing: Some("2025-01-01T00:00:00Z".to_string()),
                other: Value::Null,
            }),
            other: Value::Null,
        };
        assert!(!season.is_fully_aired());
    }

    // Season without statistics is treated as fully aired
    #[test]
    fn fully_aired_with_no_statistics() {
        let season = SeasonResource {
            season_number: 1,
            monitored: true,
            statistics: None,
            other: Value::Null,
        };
        assert!(season.is_fully_aired());
    }

    // Episode with no airDate field is considered to have aired
    #[test]
    fn episode_has_aired_no_airdate() {
        let episode = EpisodeResource {
            id: 1,
            season_number: 1,
            episode_number: 1,
            has_file: false,
            monitored: false,
            other: Value::Null,
        };
        assert!(episode.has_aired());
    }

    // Episode with past airDate is considered to have aired
    #[test]
    fn episode_has_aired_past_date() {
        let mut other = serde_json::Map::new();
        other.insert("airDate".to_string(), json!("2020-01-01T00:00:00Z"));
        let episode = EpisodeResource {
            id: 1,
            season_number: 1,
            episode_number: 1,
            has_file: false,
            monitored: false,
            other: Value::Object(other),
        };
        assert!(episode.has_aired());
    }

    // Episode with future airDate is not considered to have aired
    #[test]
    fn episode_not_aired_future_date() {
        let mut other = serde_json::Map::new();
        other.insert("airDate".to_string(), json!("2099-01-01T00:00:00Z"));
        let episode = EpisodeResource {
            id: 1,
            season_number: 1,
            episode_number: 1,
            has_file: false,
            monitored: false,
            other: Value::Object(other),
        };
        assert!(!episode.has_aired());
    }

    // Episode with invalid airDate format is considered to have aired
    #[test]
    fn episode_has_aired_invalid_date() {
        let mut other = serde_json::Map::new();
        other.insert("airDate".to_string(), json!("invalid-date"));
        let episode = EpisodeResource {
            id: 1,
            season_number: 1,
            episode_number: 1,
            has_file: false,
            monitored: false,
            other: Value::Object(other),
        };
        assert!(episode.has_aired());
    }
}
