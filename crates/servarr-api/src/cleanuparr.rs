//! Cleanuparr API client for *arr instance registration (#605).
//!
//! Cleanuparr exposes a JSON REST contract for registering Sonarr/Radarr/Lidarr/
//! Readarr/Whisparr instances:
//! `GET /api/configuration/{kind}`, `POST /api/configuration/{kind}/instances`
//! (create), `PUT /api/configuration/{kind}/instances/{id}` (update),
//! `DELETE /api/configuration/{kind}/instances/{id}` (delete) — all authenticated
//! with Cleanuparr's own API key via the `X-Api-Key` header.

use serde::{Deserialize, Serialize};

use crate::client::ApiError;
use crate::cross_app_sync::{CrossAppSync, RegisteredArrInstance};

/// *arr kinds Cleanuparr's `ArrConfigController` accepts as a path segment.
const SUPPORTED_KINDS: &[&str] = &["sonarr", "radarr", "lidarr", "readarr", "whisparr"];

/// Client for the Cleanuparr *arr-instance registration API.
#[derive(Clone, Debug)]
pub struct CleanuparrClient {
    base_url: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct ConfigurationResponse {
    instances: Vec<InstanceEntry>,
}

#[derive(Deserialize)]
struct InstanceEntry {
    name: String,
}

#[derive(Serialize)]
struct ArrInstanceRequest<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    instance_type: &'a str,
    url: &'a str,
    #[serde(rename = "apiKey")]
    api_key: &'a str,
    version: f32,
}

/// Cleanuparr's `ArrInstanceRequest.Version` is `[Required]` and, for Sonarr/Radarr/
/// Lidarr/Readarr, purely descriptive metadata -- `ArrClientFactory.GetClient` only
/// branches on it for Whisparr (2 vs 3). The value must still match each app's actual
/// API major version, confirmed against each hardcoded client's REST path in
/// Cleanuparr v2.10.5 source (`SonarrClient`/`RadarrClient`: `/api/v3/...`;
/// `LidarrClient`/`ReadarrClient`: `/api/v1/...`). Omitting the field entirely fails
/// ASP.NET Core model validation with a 400 before the request ever reaches this logic.
fn arr_kind_version(kind: &str) -> f32 {
    match kind {
        "lidarr" | "readarr" => 1.0,
        _ => 3.0,
    }
}

impl CleanuparrClient {
    /// Create a new Cleanuparr API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://cleanuparr:11011`).
    /// `api_key` is sent as the `X-Api-Key` header.
    ///
    /// # Errors
    ///
    /// Returns `ApiError::InvalidApiKey` if `api_key` contains characters that
    /// cannot be sent as an HTTP header value.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let mut value =
            reqwest::header::HeaderValue::from_str(api_key).map_err(|_| ApiError::InvalidApiKey)?;
        // Prevents the key from appearing in reqwest::Client's Debug output (which prints
        // default_headers unconditionally) if this client is ever logged.
        value.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::HeaderName::from_static("x-api-key"), value);

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .default_headers(headers)
                // No legitimate reason for Cleanuparr to redirect these calls; following one
                // would replay the X-Api-Key header cross-host (reqwest only strips
                // Authorization/Cookie/Proxy-Authorization on a cross-host redirect, not
                // custom headers).
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(ApiError::Request)?,
        })
    }

    fn validate_kind(kind: &str) -> Result<(), ApiError> {
        if SUPPORTED_KINDS.contains(&kind) {
            Ok(())
        } else {
            Err(ApiError::OperationFailed {
                message: format!("Cleanuparr does not support *arr kind: {kind}"),
            })
        }
    }
}

impl CrossAppSync for CleanuparrClient {
    /// # Errors
    ///
    /// Returns `ApiError::OperationFailed` if `kind` is not one Cleanuparr's
    /// `ArrConfigController` accepts. See [`ApiError`] for other failure modes.
    async fn list_registered(&self, kind: &str) -> Result<Vec<String>, ApiError> {
        Self::validate_kind(kind)?;
        let url = format!("{}/api/configuration/{kind}", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Cleanuparr error response body");
                String::new()
            });
            return Err(ApiError::ApiResponse { status, body });
        }

        let body: ConfigurationResponse = resp.json().await.map_err(ApiError::Request)?;
        Ok(body.instances.into_iter().map(|i| i.name).collect())
    }

    /// # Errors
    ///
    /// Returns `ApiError::OperationFailed` if `kind` is not one Cleanuparr's
    /// `ArrConfigController` accepts. See [`ApiError`] for other failure modes.
    async fn register(&self, kind: &str, instance: &RegisteredArrInstance) -> Result<(), ApiError> {
        Self::validate_kind(kind)?;
        let url = format!("{}/api/configuration/{kind}/instances", self.base_url);
        let body = ArrInstanceRequest {
            name: &instance.name,
            instance_type: kind,
            url: &instance.base_url,
            api_key: &instance.api_key,
            version: arr_kind_version(kind),
        };
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Cleanuparr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn cleanuparr_client_new_constructs() {
        let client =
            CleanuparrClient::new("http://localhost:11011", "test-key").expect("should construct");
        assert_eq!(client.base_url, "http://localhost:11011");
    }

    #[test]
    fn cleanuparr_client_new_trims_trailing_slash() {
        let client =
            CleanuparrClient::new("http://localhost:11011/", "test-key").expect("should construct");
        assert_eq!(client.base_url, "http://localhost:11011");
    }

    #[test]
    fn cleanuparr_client_debug_does_not_leak_api_key() {
        let client = CleanuparrClient::new("http://localhost:11011", "super-secret-key")
            .expect("should construct");
        let debug_output = format!("{client:?}");
        assert!(
            !debug_output.contains("super-secret-key"),
            "api key leaked into Debug output: {debug_output}"
        );
    }

    #[test]
    fn cleanuparr_client_new_invalid_api_key() {
        let result = CleanuparrClient::new("http://localhost:11011", "test\nkey");
        match result {
            Err(ApiError::InvalidApiKey) => {}
            other => panic!("expected InvalidApiKey, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_registered_calls_correct_endpoint_and_sends_api_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/configuration/sonarr"))
            .and(header("X-Api-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "instances": [{"name": "Sonarr1"}, {"name": "Sonarr2"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = CleanuparrClient::new(&server.uri(), "test-key").expect("should construct");
        let names = client.list_registered("sonarr").await.expect("should list");

        assert_eq!(names, vec!["Sonarr1".to_string(), "Sonarr2".to_string()]);
    }

    #[tokio::test]
    async fn list_registered_returns_error_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/configuration/radarr"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal error"))
            .mount(&server)
            .await;

        let client = CleanuparrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.list_registered("radarr").await.unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 500),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn list_registered_rejects_unsupported_kind() {
        let server = MockServer::start().await;
        let client = CleanuparrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.list_registered("plex").await.unwrap_err();
        match err {
            ApiError::OperationFailed { message } => assert!(message.contains("plex")),
            other => panic!("expected OperationFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn register_posts_correct_body_and_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/configuration/sonarr/instances"))
            .and(header("X-Api-Key", "test-key"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;

        let client = CleanuparrClient::new(&server.uri(), "test-key").expect("should construct");
        let instance = RegisteredArrInstance {
            name: "Sonarr1".to_string(),
            base_url: "http://sonarr:8989".to_string(),
            api_key: "sonarr-key".to_string(),
        };
        client
            .register("sonarr", &instance)
            .await
            .expect("should register");
    }

    #[tokio::test]
    async fn register_sends_required_version_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/configuration/lidarr/instances"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "name": "Lidarr1",
                "type": "lidarr",
                "url": "http://lidarr:8686",
                "apiKey": "lidarr-key",
                "version": 1.0
            })))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;

        let client = CleanuparrClient::new(&server.uri(), "test-key").expect("should construct");
        let instance = RegisteredArrInstance {
            name: "Lidarr1".to_string(),
            base_url: "http://lidarr:8686".to_string(),
            api_key: "lidarr-key".to_string(),
        };
        client
            .register("lidarr", &instance)
            .await
            .expect("should register");
    }

    #[tokio::test]
    async fn register_returns_error_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/configuration/radarr/instances"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid URL"))
            .mount(&server)
            .await;

        let client = CleanuparrClient::new(&server.uri(), "test-key").expect("should construct");
        let instance = RegisteredArrInstance {
            name: "Radarr1".to_string(),
            base_url: "invalid".to_string(),
            api_key: "key".to_string(),
        };
        let err = client.register("radarr", &instance).await.unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn register_rejects_unsupported_kind() {
        let server = MockServer::start().await;
        let client = CleanuparrClient::new(&server.uri(), "test-key").expect("should construct");
        let instance = RegisteredArrInstance {
            name: "Plex1".to_string(),
            base_url: "http://plex:32400".to_string(),
            api_key: "key".to_string(),
        };
        let err = client.register("plex", &instance).await.unwrap_err();
        match err {
            ApiError::OperationFailed { message } => assert!(message.contains("plex")),
            other => panic!("expected OperationFailed, got: {other}"),
        }
    }
}
