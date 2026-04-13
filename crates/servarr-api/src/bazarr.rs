//! Bazarr API client for subtitle manager configuration.

use std::time::Duration;

use reqwest::Client;

use crate::ApiError;

/// Client for the Bazarr subtitle management API.
#[derive(Debug, Clone)]
pub struct BazarrClient {
    base_url: String,
    api_key: String,
    http: Client,
}

impl BazarrClient {
    /// Create a new `BazarrClient`.
    ///
    /// # Errors
    ///
    /// Returns `ApiError::InvalidApiKey` if `api_key` contains non-visible-ASCII characters.
    /// Returns `ApiError::Request` if the underlying HTTP client cannot be built.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        if api_key.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
            return Err(ApiError::InvalidApiKey);
        }
        // 30s request timeout and 10s connect timeout to match other API clients.
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            http,
        })
    }

    /// Ping the Bazarr health endpoint.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` if the request fails or the server returns a non-2xx status.
    pub async fn ping(&self) -> Result<(), ApiError> {
        let url = format!("{}/api/system/ping", self.base_url);
        let resp = self
            .http
            .get(&url)
            .header("X-API-KEY", &self.api_key)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// POST form data to `/api/system/settings`.
    ///
    /// Bazarr settings are form-encoded, not JSON. The caller assembles the form fields.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` if the request fails or the server returns a non-2xx status.
    pub async fn post_settings(&self, form: &[(&str, &str)]) -> Result<(), ApiError> {
        let url = format!("{}/api/system/settings", self.base_url);
        let resp = self
            .http
            .post(&url)
            .header("X-API-KEY", &self.api_key)
            .form(form)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Configure Bazarr to use a Sonarr instance.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn configure_sonarr(
        &self,
        host: &str,
        port: u16,
        api_key: &str,
    ) -> Result<(), ApiError> {
        self.post_settings(&[
            ("settings-general-use_sonarr", "true"),
            ("settings-sonarr-ip", host),
            ("settings-sonarr-port", &port.to_string()),
            ("settings-sonarr-base_url", "/"),
            ("settings-sonarr-ssl", "false"),
            ("settings-sonarr-apikey", api_key),
        ])
        .await
    }

    /// Configure Bazarr to use a Radarr instance.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn configure_radarr(
        &self,
        host: &str,
        port: u16,
        api_key: &str,
    ) -> Result<(), ApiError> {
        self.post_settings(&[
            ("settings-general-use_radarr", "true"),
            ("settings-radarr-ip", host),
            ("settings-radarr-port", &port.to_string()),
            ("settings-radarr-base_url", "/"),
            ("settings-radarr-ssl", "false"),
            ("settings-radarr-apikey", api_key),
        ])
        .await
    }

    /// Disable Sonarr in Bazarr.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn disable_sonarr(&self) -> Result<(), ApiError> {
        self.post_settings(&[("settings-general-use_sonarr", "false")])
            .await
    }

    /// Disable Radarr in Bazarr.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn disable_radarr(&self) -> Result<(), ApiError> {
        self.post_settings(&[("settings-general-use_radarr", "false")])
            .await
    }

    /// Set Bazarr admin credentials (form login).
    ///
    /// `password_md5` must be the MD5 hex digest of the plaintext password — Bazarr
    /// stores and compares the MD5 hash, not the plaintext.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn set_credentials(
        &self,
        username: &str,
        password_md5: &str,
    ) -> Result<(), ApiError> {
        self.post_settings(&[
            ("settings-auth-type", "form"),
            ("settings-auth-username", username),
            ("settings-auth-password", password_md5),
        ])
        .await
    }
}
