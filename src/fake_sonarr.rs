use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

pub struct FakeSonarr {
    state: Arc<Mutex<SonarrState>>,
    url: String,
}

struct SonarrState {
    series: Vec<Value>,
    episodes: Vec<Value>,
    tags: Vec<Value>,
    commands: Vec<Value>,
}

impl FakeSonarr {
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(SonarrState {
            series: Vec::new(),
            episodes: Vec::new(),
            tags: Vec::new(),
            commands: Vec::new(),
        }));

        let router = Router::new()
            .route("/api", get(probe))
            .route("/api/v3/series", get(get_series))
            .route("/api/v3/series/{id}", put(put_series))
            .route("/api/v3/tag", get(get_tags))
            .route("/api/v3/episode", get(get_episodes))
            .route("/api/v3/episode/monitor", put(put_episode_monitor))
            .route("/api/v3/command", post(post_command))
            .with_state(state.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());

        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        Self { state, url }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn add_series(&self, series: Value) {
        self.state.lock().unwrap().series.push(series);
    }

    pub fn add_episodes(&self, episodes: Vec<Value>) {
        self.state.lock().unwrap().episodes.extend(episodes);
    }

    #[allow(dead_code)]
    pub fn add_tag(&self, id: i32, label: &str) {
        self.state
            .lock()
            .unwrap()
            .tags
            .push(json!({"id": id, "label": label}));
    }

    pub fn commands(&self) -> Vec<Value> {
        self.state.lock().unwrap().commands.clone()
    }

    pub fn episode(&self, id: i32) -> Value {
        self.state
            .lock()
            .unwrap()
            .episodes
            .iter()
            .find(|e| e["id"].as_i64() == Some(i64::from(id)))
            .cloned()
            .unwrap_or(Value::Null)
    }

    pub fn series_state(&self, id: i32) -> Value {
        self.state
            .lock()
            .unwrap()
            .series
            .iter()
            .find(|s| s["id"].as_i64() == Some(i64::from(id)))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

pub fn make_series(id: i32, title: &str, tvdb_id: i32, seasons: &[Value]) -> Value {
    json!({
        "id": id,
        "title": title,
        "tvdbId": tvdb_id,
        "monitored": false,
        "monitorNewItems": "all",
        "seasons": seasons,
    })
}

pub fn make_season(number: i32, monitored: bool, fully_aired: bool) -> Value {
    json!({
        "seasonNumber": number,
        "monitored": monitored,
        "statistics": {
            "sizeOnDisk": 9000,
            "episodeCount": 8,
            "episodeFileCount": 8,
            "totalEpisodeCount": 8,
            "nextAiring": if fully_aired { Value::Null } else { json!("2025-06-01T00:00:00Z") },
        }
    })
}

pub fn make_episode(id: i32, series_id: i32, season: i32, episode: i32, has_file: bool) -> Value {
    json!({
        "id": id,
        "seriesId": series_id,
        "seasonNumber": season,
        "episodeNumber": episode,
        "hasFile": has_file,
        "monitored": false,
    })
}

async fn probe() -> &'static str {
    "ok"
}

async fn get_series(State(state): State<Arc<Mutex<SonarrState>>>) -> Json<Value> {
    Json(Value::Array(state.lock().unwrap().series.clone()))
}

// Sonarr behavior: PUT /api/v3/series/{id} updates the series. When a season's
// `monitored` flips from false to true, Sonarr calls SetEpisodeMonitoredBySeason
// which sets ALL episodes in that season to monitored.
// See: NzbDrone.Core/Tv/SeriesService.cs UpdateSeries()
async fn put_series(
    State(state): State<Arc<Mutex<SonarrState>>>,
    Path(id): Path<i32>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut state = state.lock().unwrap();
    let id_val = i64::from(id);

    if let Some(pos) = state
        .series
        .iter()
        .position(|s| s["id"].as_i64() == Some(id_val))
    {
        let newly_monitored: Vec<i64> = body["seasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|new_season| new_season["monitored"].as_bool() == Some(true))
            .filter(|new_season| {
                let sn = new_season["seasonNumber"].as_i64();
                let old_seasons = state.series[pos]["seasons"].as_array();
                let was_monitored = old_seasons
                    .and_then(|ss| ss.iter().find(|s| s["seasonNumber"].as_i64() == sn))
                    .and_then(|s| s["monitored"].as_bool())
                    .unwrap_or(false);
                !was_monitored
            })
            .filter_map(|s| s["seasonNumber"].as_i64())
            .collect();

        for sn in newly_monitored {
            for ep in &mut state.episodes {
                if ep["seriesId"].as_i64() == Some(id_val)
                    && ep["seasonNumber"].as_i64() == Some(sn)
                {
                    ep["monitored"] = Value::Bool(true);
                }
            }
        }

        state.series[pos] = body;
    }

    Json(json!({}))
}

async fn get_tags(State(state): State<Arc<Mutex<SonarrState>>>) -> Json<Value> {
    Json(Value::Array(state.lock().unwrap().tags.clone()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpisodeQuery {
    series_id: i32,
    season_number: Option<i32>,
}

async fn get_episodes(
    State(state): State<Arc<Mutex<SonarrState>>>,
    Query(q): Query<EpisodeQuery>,
) -> Json<Value> {
    let state = state.lock().unwrap();
    let series_id = i64::from(q.series_id);
    let filtered: Vec<_> = state
        .episodes
        .iter()
        .filter(|e| e["seriesId"].as_i64() == Some(series_id))
        .filter(|e| {
            q.season_number
                .is_none_or(|sn| e["seasonNumber"].as_i64() == Some(i64::from(sn)))
        })
        .cloned()
        .collect();
    Json(Value::Array(filtered))
}

async fn put_episode_monitor(
    State(state): State<Arc<Mutex<SonarrState>>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut state = state.lock().unwrap();
    let ids = body["episodeIds"].as_array().unwrap();
    let monitored = body["monitored"].as_bool().unwrap();

    for ep in &mut state.episodes {
        if ids.iter().any(|id| id.as_i64() == ep["id"].as_i64()) {
            ep["monitored"] = Value::Bool(monitored);
        }
    }

    Json(json!([]))
}

// Sonarr behavior for search commands:
//
// SeasonSearch (SeasonSearchService.cs) calls ReleaseSearchService.SeasonSearch
// with monitoredOnly=true. The MonitoredEpisodeSpecification decision engine
// check will reject any release where:
//   - series.monitored is false, OR
//   - any episode in the release has monitored=false
// Season-level monitoring is NOT checked by the decision engine; it only matters
// as a propagation mechanism (flipping season.monitored monitors all episodes).
//
// EpisodeSearch (EpisodeSearchService.cs) calls ReleaseSearchService.EpisodeSearch
// with monitoredOnly=false (hardcoded). The MonitoredEpisodeSpecification skips
// its check entirely when monitoredOnly=false, so unmonitored episodes CAN be
// grabbed via EpisodeSearch.
//
// This means: before issuing a SeasonSearch, the caller must ensure the series
// and all episodes in the season are monitored. EpisodeSearch has no such
// requirement.
async fn post_command(
    State(state): State<Arc<Mutex<SonarrState>>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut state = state.lock().unwrap();

    if body["name"].as_str() == Some("SeasonSearch") {
        let series_id = body["seriesId"].as_i64().unwrap();
        let season_number = body["seasonNumber"].as_i64().unwrap();

        let series = state
            .series
            .iter()
            .find(|s| s["id"].as_i64() == Some(series_id));

        assert!(
            series.is_some_and(|s| s["monitored"].as_bool() == Some(true)),
            "SeasonSearch requires the series to be monitored"
        );

        let unmonitored: Vec<i64> = state
            .episodes
            .iter()
            .filter(|e| {
                e["seriesId"].as_i64() == Some(series_id)
                    && e["seasonNumber"].as_i64() == Some(season_number)
            })
            .filter(|e| e["monitored"].as_bool() != Some(true))
            .filter_map(|e| e["episodeNumber"].as_i64())
            .collect();

        assert!(
            unmonitored.is_empty(),
            "SeasonSearch for season {season_number} would be rejected by Sonarr: \
             episodes {unmonitored:?} are not monitored"
        );
    }

    state.commands.push(body);
    Json(json!({}))
}
