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
    /// The API returned HTTP success but a body indicating the operation failed.
    /// Maintainerr wraps mutating responses in a `{ status: "NOK", message }`
    /// envelope and still answers 200, so a success status alone is not enough (#156).
    #[error("operation rejected by API: {message}")]
    OperationFailed { message: String },
}

impl ApiError {
    /// Returns a log-safe summary that excludes response body content.
    ///
    /// Use for credential-bearing API calls where the response body from the
    /// downstream API may echo back the submitted credential (API keys, tokens,
    /// passwords) in a validation error message.
    ///
    /// Deliberately an exhaustive match with no wildcard arm: a future variant that
    /// carries response-body content must add its own case here rather than silently
    /// falling through to `Display` (which is unsanitized by default). `Request` gets
    /// its own reduction rather than `self.to_string()` — `reqwest::Error`'s `Display`
    /// appends the full request URL, and Sabnzbd/Tautulli send API keys and admin
    /// passwords as query parameters, so that URL can carry credentials.
    /// `OperationFailed`'s `message` is upstream-controlled response content
    /// (Maintainerr's `{status:"NOK", message}` envelope) and can echo a submitted
    /// `apiKey` back the same way.
    pub fn log_summary(&self) -> String {
        match self {
            Self::ApiResponse { status, .. } => format!("HTTP API error (status: {status})"),
            Self::Request(e) => format!(
                "HTTP request failed ({})",
                if e.is_timeout() {
                    "timeout"
                } else if e.is_connect() {
                    "connect"
                } else if e.is_decode() {
                    "decode"
                } else {
                    "transport"
                }
            ),
            Self::OperationFailed { .. } => "operation rejected by API".to_string(),
            Self::InvalidUrl(_) => "invalid base URL".to_string(),
            Self::InvalidApiKey => Self::InvalidApiKey.to_string(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_summary_hides_response_body_for_api_response() {
        let err = ApiError::ApiResponse {
            status: 401,
            body: "invalid api key SUPER-SECRET-KEY".to_string(),
        };
        let summary = err.log_summary();
        assert!(summary.contains("401"));
        assert!(!summary.contains("SUPER-SECRET-KEY"));
    }

    #[test]
    fn log_summary_hides_message_for_operation_failed() {
        // The Maintainerr {status:"NOK", message} envelope lifts `message` verbatim from
        // the upstream response body (#398 follow-up).
        let err = ApiError::OperationFailed {
            message: "rejected apiKey=SUPER-SECRET-KEY".to_string(),
        };
        let summary = err.log_summary();
        assert_eq!(summary, "operation rejected by API");
        assert!(!summary.contains("SUPER-SECRET-KEY"));
    }

    #[test]
    fn log_summary_invalid_api_key_matches_display() {
        let err = ApiError::InvalidApiKey;
        assert_eq!(err.log_summary(), err.to_string());
    }

    /// Regression test for the credential-leak this fix closes: `reqwest::Error`'s `Display`
    /// includes the full request URL (verified against the pinned `reqwest` version), and
    /// Sabnzbd/Tautulli send credentials as query parameters — so `ApiError::Request` reaching
    /// `log_summary()` used to leak them via the previous wildcard-arm passthrough.
    #[tokio::test]
    async fn log_summary_hides_url_for_request_error() {
        // Port 1 is reserved and never listening, guaranteeing a connect failure without
        // depending on external network state.
        let client = HttpClient::new("http://127.0.0.1:1", None).expect("valid base url");
        let path =
            "?mode=set_config&keyword=password&value=SUPER-SECRET-PASSWORD&apikey=SUPER-SECRET-KEY";
        let result: Result<serde_json::Value, ApiError> = client.get(path).await;

        let err = result.expect_err("connection to port 1 must fail");
        assert!(
            matches!(err, ApiError::Request(_)),
            "expected ApiError::Request, got {err:?}"
        );

        // The raw error still carries the URL (that's the bug this fix guards against) —
        // assert the precondition holds, so this test would fail loudly if reqwest ever
        // stopped including it, rather than passing for the wrong reason.
        assert!(err.to_string().contains("SUPER-SECRET-KEY"));

        let summary = err.log_summary();
        assert!(!summary.contains("SUPER-SECRET-KEY"));
        assert!(!summary.contains("SUPER-SECRET-PASSWORD"));
        assert!(!summary.contains("127.0.0.1"));
    }
}
