use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Client for the Plex Media Server API.
///
/// Plex exposes a `GET /identity` endpoint that returns an XML document
/// with server identity information (HTTP 200) when running. No API key required.
#[derive(Debug, Clone)]
pub struct PlexClient {
    http: HttpClient,
}

impl PlexClient {
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        Ok(Self {
            http: HttpClient::new(base_url, None)?,
        })
    }
}

impl HealthCheck for PlexClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let url = self.http.base_url().join("/identity")?;
        let resp = self.http.inner().get(url).send().await?;
        Ok(resp.status().is_success())
    }
}
