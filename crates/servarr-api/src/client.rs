use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Invalid base URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("API returned {status}: {body}")]
    ApiResponse { status: u16, body: String },
    #[error("API key contains invalid characters (non-visible ASCII)")]
    InvalidApiKey,
}

/// Shared HTTP client for all Servarr-family API interactions.
///
/// Wraps [`reqwest::Client`] with a base URL and optional API key.
/// All service-specific clients (Sonarr, Radarr, Transmission, etc.)
/// build on top of this.
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    base_url: Url,
}

impl HttpClient {
    /// Create a new client for the given base URL and optional API key.
    ///
    /// The API key is sent as the `X-Api-Key` header on every request
    /// (the standard header for Sonarr/Radarr/Prowlarr/Lidarr).
    pub fn new(base_url: &str, api_key: Option<&str>) -> Result<Self, ApiError> {
        let base_url = Url::parse(base_url)?;

        let mut headers = HeaderMap::new();
        if let Some(key) = api_key {
            headers.insert(
                "X-Api-Key",
                HeaderValue::from_str(key).map_err(|_| ApiError::InvalidApiKey)?,
            );
        }

        let inner = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()?;

        Ok(Self { inner, base_url })
    }

    /// GET `{base_url}/{path}` and deserialize the JSON response.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let url = self.base_url.join(path)?;
        let resp = self.inner.get(url).send().await?;
        Self::handle_response(resp).await
    }

    /// POST `{base_url}/{path}` with a JSON body and deserialize the response.
    pub async fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let url = self.base_url.join(path)?;
        let resp = self.inner.post(url).json(body).send().await?;
        Self::handle_response(resp).await
    }

    /// DELETE `{base_url}/{path}`.
    pub async fn delete(&self, path: &str) -> Result<(), ApiError> {
        let url = self.base_url.join(path)?;
        let resp = self.inner.delete(url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::ApiResponse { status, body });
        }
        Ok(())
    }

    /// PUT `{base_url}/{path}` with a JSON body and deserialize the response.
    pub async fn put<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let url = self.base_url.join(path)?;
        let resp = self.inner.put(url).json(body).send().await?;
        Self::handle_response(resp).await
    }

    /// Return a reference to the underlying [`reqwest::Client`] for
    /// advanced use cases (e.g. Transmission RPC with custom headers).
    pub fn inner(&self) -> &reqwest::Client {
        &self.inner
    }

    /// Return the base URL.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    async fn handle_response<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, ApiError> {
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::ApiResponse { status, body });
        }
        Ok(resp.json().await?)
    }
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("base_url", &self.base_url.as_str())
            .finish()
    }
}
