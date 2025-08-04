use anyhow::{Context, Result, anyhow};
use reqwest::{
    Url,
    header::{HeaderMap, HeaderValue},
};
use rustls_platform_verifier::ConfigVerifierExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracing::{debug, error, info, warn};

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
            .use_preconfigured_tls(rustls::ClientConfig::with_platform_verifier())
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

    pub async fn series(&self) -> Result<Vec<SeriesResource>> {
        let series = self
            .get::<Value, ()>("series", None)
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

    pub async fn monitor_episodes(
        &self,
        episodes: &[EpisodeResource],
    ) -> Result<serde_json::Value> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("url is relative"))?
            .push("api")
            .push("v3")
            .push("episode")
            .push("monitor");

        let episode_ids: Vec<_> = episodes.iter().map(|e| e.id).collect();
        let request = EpisodeMonitoredResource {
            episode_ids,
            monitored: true,
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

    pub async fn episodes(
        &self,
        series: &SeriesResource,
        season_start: i32,
        episode_start: i32,
        num: usize,
    ) -> Result<Vec<EpisodeResource>> {
        let episodes = self
            .get::<Vec<EpisodeResource>, _>("episode", Some(&[("seriesId", series.id)]))
            .await
            .context("error fetching episodes")?;

        let episodes = episode_window(season_start, episode_start, num, episodes);

        Ok(episodes)
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
        series: &SeriesResource,
        season_num: i32,
    ) -> Result<serde_json::Value> {
        info!(num = season_num, "Searching season");

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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonStatisticsResource {
    pub size_on_disk: i64,
    pub episode_count: i32,
    pub episode_file_count: i32,
    pub total_episode_count: i32,
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

    pub fn monitor_seasons(&mut self, seasons: &[i32]) -> bool {
        let mut changed = false;
        for &season_id in seasons {
            if let Some(season) = self.season_mut(season_id) {
                changed |= !season.monitored;
                season.monitored = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod test {
    use httpmock::Method::{POST, PUT};
    use serde_json::{Value, json};

    use crate::sonarr::{
        EpisodeResource, NewItemMonitorTypes, SeasonResource, SeasonStatisticsResource,
        SeriesResource,
    };

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
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

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
                episode_file_count: 8,
                total_episode_count: 0,
                other: Value::Null,
            }
            .into(),
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
                                    "totalEpisodeCount": 0,
                                }
                            }]
                        }
                    ));
                then.json_body(json!({}));
            })
            .await;
        let client = super::Client::new(&server.url("/pathprefix"), "secret")?;

        client.search_season(&series, 1).await?;

        series_mock.assert_async().await;
        command_mock.assert_async().await;

        Ok(())
    }

    #[test]
    fn episode_window_none() {
        let episodes = vec![EpisodeResource {
            ..default_episode()
        }];

        assert!(super::episode_window(1, 1, 0, episodes.clone()).is_empty());
        assert!(super::episode_window(1, 1, 1, episodes.clone()).is_empty());
    }

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
}
