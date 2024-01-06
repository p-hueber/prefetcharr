use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;

pub struct Client {
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(mut base_url: String, api_key: String) -> Self {
        if !base_url.ends_with('/') {
            base_url += "/";
        }
        Self { base_url, api_key }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = reqwest::get(format!(
            "{}api/v3/{}?apikey={}",
            self.base_url, path, self.api_key
        ))
        .await?;
        Ok(response.json::<T>().await?)
    }

    pub async fn put_series(&self, series: &SeriesResource) -> Result<serde_json::Value> {
        let response = reqwest::Client::new()
            .put(format!(
                "{}api/v3/series/{}?apikey={}",
                self.base_url, series.id, self.api_key
            ))
            .json(series)
            .send()
            .await?;
        Ok(response.json().await?)
    }

    pub async fn series(&self) -> Result<Vec<SeriesResource>> {
        self.get("series").await
    }

    pub async fn search_season(
        &self,
        series: &SeriesResource,
        season_num: i32,
    ) -> Result<serde_json::Value> {
        let series_monitored = series.monitored;

        let mut series = series.clone();
        let season = series
            .season_mut(season_num)
            .ok_or_else(|| anyhow!("there is no season {season_num}"))?;

        if !season.monitored || !series_monitored {
            season.monitored = true;
            series.monitored = true;
            self.put_series(&series).await?;
        }

        let cmd = json!({
            "name": "SeasonSearch",
            "seriesId": series.id,
            "seasonNumber": season_num,
        });

        let response = reqwest::Client::new()
            .post(format!(
                "{}api/v3/command?apikey={}",
                self.base_url, self.api_key
            ))
            .json(&cmd)
            .send()
            .await?;
        Ok(response.json().await?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonStatisticsResource {
    pub size_on_disk: i64,
    pub episode_count: i32,
    pub total_episode_count: i32,
    #[serde(flatten)]
    other: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonResource {
    pub season_number: i32,
    pub monitored: bool,
    pub statistics: SeasonStatisticsResource,
    #[serde(flatten)]
    other: serde_json::Value,
}

impl SeasonResource {
    pub fn last_episode(&self) -> i32 {
        self.statistics.total_episode_count
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
    #[serde(flatten)]
    other: serde_json::Value,
}

impl SeriesResource {
    pub fn season(&self, num: i32) -> Option<&SeasonResource> {
        self.seasons.iter().find(|s| s.season_number == num)
    }

    pub fn season_mut(&mut self, num: i32) -> Option<&mut SeasonResource> {
        self.seasons.iter_mut().find(|s| s.season_number == num)
    }
}
