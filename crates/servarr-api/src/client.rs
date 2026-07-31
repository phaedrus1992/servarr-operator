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
    /// Matches exhaustively (no wildcard) so a new variant is a compile error here,
    /// not a silent leak. `Request` used to fall through a wildcard to
    /// `reqwest::Error`'s `Display`, which appends the full request URL — several
    /// clients (Sabnzbd, Tautulli) send API keys and admin passwords as query
    /// parameters, so that URL can carry credentials. `OperationFailed` used to fall
    /// through the same wildcard; its `message` is upstream-controlled response
    /// content (Maintainerr's NOK envelope) and can echo a submitted `apiKey` back.
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
    use crate::test_support::{SEED_TOKEN, is_tenant_safe_charset};
    use proptest::prelude::*;

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
        let err = ApiError::OperationFailed {
            message: "rejected apiKey=SUPER-SECRET-KEY".to_string(),
        };
        let summary = err.log_summary();
        assert_eq!(summary, "operation rejected by API");
        assert!(!summary.contains("SUPER-SECRET-KEY"));
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
        // The summary must be one of the four fixed forms — never the request URL.
        assert!(
            matches!(
                summary.as_str(),
                "HTTP request failed (timeout)"
                    | "HTTP request failed (connect)"
                    | "HTTP request failed (decode)"
                    | "HTTP request failed (transport)"
            ),
            "unexpected Request summary: {summary}"
        );
        assert!(
            is_tenant_safe_charset(&summary),
            "summary has chars outside the allowlist: {summary}"
        );
    }

    #[test]
    fn log_summary_invalid_api_key_is_fixed_and_charset_safe() {
        let summary = ApiError::InvalidApiKey.log_summary();
        assert_eq!(
            summary,
            "API key contains invalid characters (non-visible ASCII)"
        );
        assert!(is_tenant_safe_charset(&summary));
    }

    proptest! {
        // The response body from a credential-bearing call can echo the submitted
        // API key; `log_summary` must drop it while keeping the status code.
        #[test]
        fn log_summary_api_response_never_leaks_body(
            status in any::<u16>(),
            seed in any::<String>(),
        ) {
            let seed = format!("{SEED_TOKEN}{seed}");
            let err = ApiError::ApiResponse {
                status,
                body: format!("invalid api key {seed}"),
            };
            let summary = err.log_summary();
            prop_assert!(
                summary.contains(&status.to_string()),
                "status code must be preserved: {summary}"
            );
            prop_assert!(
                !summary.contains(&seed),
                "response body leaked into summary: {summary}"
            );
            prop_assert!(is_tenant_safe_charset(&summary));
        }
    }

    // Maintainerr's NOK envelope `message` is upstream-controlled and can echo a
    // submitted `apiKey`; `log_summary` must collapse it to the fixed string. The
    // output is constant, so a single fixed seed exercises the same no-leak and
    // collapse guarantee the property loop would.
    #[test]
    fn log_summary_operation_failed_collapses_to_fixed_string() {
        let err = ApiError::OperationFailed {
            message: format!("rejected apiKey={SEED_TOKEN}"),
        };
        let summary = err.log_summary();
        assert_eq!(summary, "operation rejected by API");
        assert!(!summary.contains(SEED_TOKEN));
        assert!(is_tenant_safe_charset(&summary));
    }

    // `url::ParseError` carries no echo of the invalid input, so the meaningful
    // guarantee is the fixed-output collapse: whatever the parse error, the
    // summary is exactly the generic string and never reproduces the input.
    #[test]
    fn log_summary_invalid_url_collapses_to_fixed_string() {
        // A non-numeric port is a guaranteed parse failure (the `url` parser
        // rejects a non-digit in the port position).
        let parse_err = Url::parse(&format!("http://{SEED_TOKEN}:not-a-port"))
            .expect_err("non-numeric port must fail to parse");
        let err = ApiError::InvalidUrl(parse_err);
        let summary = err.log_summary();
        assert_eq!(summary, "invalid base URL");
        assert!(is_tenant_safe_charset(&summary));
    }
}
