use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

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
        let series = self
            .get::<Value>("series")
            .await?
            .as_array()
            .ok_or_else(|| anyhow!("not an array"))?
            .iter()
            .filter_map(|s| match serde_json::from_value(s.clone()) {
                Ok(v) => Some(v),
                Err(e) => {
                    debug!(series=?s, "ignoring malformed series entry: {e}");
                    None
                }
            })
            .collect::<Vec<SeriesResource>>();
        Ok(series)
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

#[cfg(test)]
mod test {
    use httpmock::Method::{POST, PUT};
    use serde_json::{json, Value};

    use crate::sonarr::{
        NewItemMonitorTypes, SeasonResource, SeasonStatisticsResource, SeriesResource,
    };

    #[tokio::test]
    async fn series_v3() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series")
                    .query_param("apikey", "secret");
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
        let client = super::Client::new(server.url("/pathprefix"), "secret".to_string());

        let series = client.series().await?;
        assert_eq!(series[0].id, 1234);

        series_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn series_multiple() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series")
                    .query_param("apikey", "secret");
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
        let client = super::Client::new(server.url("/pathprefix"), "secret".to_string());

        let series = client.series().await?;
        assert_eq!(series.len(), 2);

        series_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn series_skip_missing_statistics() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series")
                    .query_param("apikey", "secret");
                then.json_body(serde_json::json!(
                    [{
                        "id": 1234,
                        "title": "TestShow",
                        "tvdbId": 5678,
                        "monitored": false,
                        "monitorNewItems": "all",
                        "seasons": [{
                            "seasonNumber": 1,
                            "monitored": false
                        }]
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
        let client = super::Client::new(server.url("/pathprefix"), "secret".to_string());

        let series = client.series().await?;
        assert_eq!(series.len(), 1);

        series_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn series_emtpy() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series")
                    .query_param("apikey", "secret");
                then.json_body(serde_json::json!([]));
            })
            .await;
        let client = super::Client::new(server.url("/pathprefix"), "secret".to_string());

        let series = client.series().await?;
        assert_eq!(series.len(), 0);

        series_mock.assert_async().await;

        Ok(())
    }

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
            other: Value::Null,
        };

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .query_param("apikey", "secret")
                    .method(PUT)
                    .json_body(serde_json::json!(
                        {
                            "id": 1234,
                            "title": "TestShow",
                            "tvdbId": 5678,
                            "monitored": false,
                            "monitorNewItems": "all",
                            "seasons": []
                        }
                    ));
                then.json_body(json!({}));
            })
            .await;
        let client = super::Client::new(server.url("/pathprefix"), "secret".to_string());

        client.put_series(&series).await?;

        series_mock.assert_async().await;

        Ok(())
    }

    #[tokio::test]
    async fn search_season() -> Result<(), Box<dyn std::error::Error>> {
        let server = httpmock::MockServer::start_async().await;

        let season = SeasonResource {
            season_number: 1,
            monitored: false,
            statistics: SeasonStatisticsResource {
                size_on_disk: 9000,
                episode_count: 8,
                total_episode_count: 0,
                other: Value::Null,
            },
            other: Value::Null,
        };

        let series = SeriesResource {
            id: 1234,
            title: Some("TestShow".to_string()),
            tvdb_id: 5678,
            monitored: false,
            monitor_new_items: Some(NewItemMonitorTypes::All),
            seasons: vec![season],
            other: serde_json::json!({}),
        };

        let command_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/command")
                    .query_param("apikey", "secret")
                    .method(POST)
                    .json_body(json!({
                        "name": "SeasonSearch",
                        "seriesId": 1234,
                        "seasonNumber": 1,
                    }));
                then.json_body(json!({}));
            })
            .await;

        let series_mock = server
            .mock_async(|when, then| {
                when.path("/pathprefix/api/v3/series/1234")
                    .query_param("apikey", "secret")
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
                                    "totalEpisodeCount": 0,
                                }
                            }]
                        }
                    ));
                then.json_body(json!({}));
            })
            .await;
        let client = super::Client::new(server.url("/pathprefix"), "secret".to_string());

        client.search_season(&series, 1).await?;

        series_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }
}
