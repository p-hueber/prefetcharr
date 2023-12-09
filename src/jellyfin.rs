use jellyfin_api::types::SessionInfo;

pub struct Client {
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(mut base_url: String, api_key: String) -> Self {
        if !base_url.ends_with("/") {
            base_url += "/";
        }
        Self { base_url, api_key }
    }

    pub async fn sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
        let sessions =
            reqwest::get(format!("{}Sessions?apikey={}", self.base_url, self.api_key)).await?;
        Ok(sessions.json::<Vec<SessionInfo>>().await?)
    }
}
