use serde::{Deserialize, Serialize};

use crate::client::ApiError;

/// Client for the Maintainerr auto-configuration API.
///
/// Maintainerr provides a REST API to auto-configure Sonarr, Radarr, Overseerr,
/// Tautulli, and Plex via POST/PUT endpoints for server settings.
#[derive(Clone, Debug)]
pub struct MaintainerrClient {
    base_url: String,
    client: reqwest::Client,
}

/// Request body for adding a Sonarr server.
#[derive(Serialize)]
struct SonarrAddRequest<'a> {
    #[serde(rename = "serverName")]
    name: &'a str,
    url: &'a str,
    #[serde(rename = "apiKey")]
    api_key: &'a str,
}

/// Request body for adding a Radarr server.
#[derive(Serialize)]
struct RadarrAddRequest<'a> {
    #[serde(rename = "serverName")]
    name: &'a str,
    url: &'a str,
    #[serde(rename = "apiKey")]
    api_key: &'a str,
}

/// Request body for setting Overseerr configuration.
#[derive(Serialize)]
struct OverseerrSetRequest<'a> {
    url: &'a str,
    #[serde(rename = "api_key")]
    api_key: &'a str,
}

/// Request body for setting Tautulli configuration.
#[derive(Serialize)]
struct TautulliSetRequest<'a> {
    url: &'a str,
    #[serde(rename = "api_key")]
    api_key: &'a str,
}

/// Request body for setting Plex configuration and auth token.
#[derive(Serialize)]
struct PlexSetRequest<'a> {
    #[serde(rename = "plexHostname")]
    plex_hostname: &'a str,
    #[serde(rename = "plexPort")]
    plex_port: u16,
    #[serde(rename = "plexAuthToken")]
    plex_auth_token: &'a str,
}

/// Generic API response for server listings.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServerResponse {
    pub id: Option<i32>,
    #[serde(rename = "serverName", alias = "name")]
    pub name: String,
    pub url: String,
}

impl MaintainerrClient {
    /// Create a new Maintainerr API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://maintainerr:6246`).
    /// `api_key` is sent as the `X-Api-Key` header.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let base_url = base_url.trim_end_matches('/').to_string();

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static("x-api-key"),
            reqwest::header::HeaderValue::from_str(api_key).map_err(|_| ApiError::InvalidApiKey)?,
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(ApiError::Request)?;

        Ok(Self { base_url, client })
    }

    // ===== Sonarr Methods =====

    /// Add a new Sonarr server.
    pub async fn add_sonarr(
        &self,
        name: &str,
        url: &str,
        api_key: &str,
    ) -> Result<ServerResponse, ApiError> {
        let endpoint = format!("{}/api/settings/sonarr", self.base_url);
        let body = SonarrAddRequest { name, url, api_key };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            resp.json().await.map_err(ApiError::Request)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Maintainerr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// List all Sonarr servers.
    pub async fn list_sonarr(&self) -> Result<Vec<ServerResponse>, ApiError> {
        let endpoint = format!("{}/api/settings/sonarr", self.base_url);

        let resp = self
            .client
            .get(&endpoint)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            resp.json().await.map_err(ApiError::Request)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Maintainerr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }

    // ===== Radarr Methods =====

    /// Add a new Radarr server.
    pub async fn add_radarr(
        &self,
        name: &str,
        url: &str,
        api_key: &str,
    ) -> Result<ServerResponse, ApiError> {
        let endpoint = format!("{}/api/settings/radarr", self.base_url);
        let body = RadarrAddRequest { name, url, api_key };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            resp.json().await.map_err(ApiError::Request)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Maintainerr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// List all Radarr servers.
    pub async fn list_radarr(&self) -> Result<Vec<ServerResponse>, ApiError> {
        let endpoint = format!("{}/api/settings/radarr", self.base_url);

        let resp = self
            .client
            .get(&endpoint)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            resp.json().await.map_err(ApiError::Request)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Maintainerr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }

    // ===== Overseerr Methods =====

    /// Set Overseerr configuration.
    pub async fn set_overseerr(&self, url: &str, api_key: &str) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings/overseerr", self.base_url);
        let body = OverseerrSetRequest { url, api_key };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Maintainerr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }

    // ===== Tautulli Methods =====

    /// Set Tautulli configuration.
    pub async fn set_tautulli(&self, url: &str, api_key: &str) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings/tautulli", self.base_url);
        let body = TautulliSetRequest { url, api_key };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Maintainerr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }

    // ===== Plex Methods =====

    /// Set Plex configuration and auth token via a single `POST /api/settings`.
    ///
    /// **Safety note (#152):** Maintainerr's `POST /api/settings` semantics (merge vs. replace)
    /// must be confirmed. If it performs a full-document replace, this call will zero out
    /// unrelated settings (Sonarr/Radarr/Overseerr/Tautulli URLs and keys). Assumption:
    /// the endpoint is merge-aware and only updates the Plex fields.
    /// If this assumption is invalidated, switch to a Plex-specific endpoint or use
    /// fetch-then-patch (read all settings, merge Plex fields, write back).
    ///
    /// Hostname/port and the auth token are sent in one request rather than two separate
    /// calls to this same endpoint, so an uncertain replace-semantics call can't clobber
    /// what the other call just wrote.
    pub async fn set_plex(&self, hostname: &str, port: u16, auth_token: &str) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings", self.base_url);
        let body = PlexSetRequest {
            plex_hostname: hostname,
            plex_port: port,
            plex_auth_token: auth_token,
        };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Maintainerr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn server_response_accepts_servername_key() {
        // Maintainerr returns the canonical `serverName` key.
        let json = r#"{"id":1,"serverName":"Sonarr","url":"http://sonarr:8989"}"#;
        let parsed: ServerResponse = serde_json::from_str(json).expect("should parse serverName");
        assert_eq!(parsed.name, "Sonarr");
    }

    #[test]
    fn server_response_accepts_name_alias() {
        // The `name` alias must also deserialize, since list endpoints vary.
        let json = r#"{"id":2,"name":"Radarr","url":"http://radarr:7878"}"#;
        let parsed: ServerResponse = serde_json::from_str(json).expect("should parse name alias");
        assert_eq!(parsed.name, "Radarr");
    }

    #[test]
    fn server_response_serializes_to_servername() {
        // Re-serialization must always emit `serverName`, never the alias.
        let resp = ServerResponse {
            id: Some(3),
            name: "Lidarr".to_string(),
            url: "http://lidarr:8686".to_string(),
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"serverName\":\"Lidarr\""), "got: {json}");
        assert!(
            !json.contains("\"name\""),
            "alias leaked into output: {json}"
        );
    }

    #[test]
    fn maintainerr_client_new_constructs() {
        let client =
            MaintainerrClient::new("http://localhost:6246", "test-key").expect("should construct");
        assert_eq!(client.base_url, "http://localhost:6246");
    }

    #[test]
    fn maintainerr_client_new_trims_trailing_slash() {
        let client =
            MaintainerrClient::new("http://localhost:6246/", "test-key").expect("should construct");
        assert_eq!(client.base_url, "http://localhost:6246");
    }

    #[test]
    fn maintainerr_client_new_invalid_api_key() {
        let result = MaintainerrClient::new("http://localhost:6246", "test\nkey");
        match result {
            Err(ApiError::InvalidApiKey) => {}
            other => panic!("expected InvalidApiKey, got: {other:?}"),
        }
    }

    #[test]
    fn plex_set_request_serializes_correctly() {
        let req = PlexSetRequest {
            plex_hostname: "plex.example.com",
            plex_port: 32400,
            plex_auth_token: "my-plex-token",
        };
        let json = serde_json::to_value(&req).expect("should serialize");
        assert_eq!(json["plexHostname"], "plex.example.com");
        assert_eq!(json["plexPort"], 32400);
        assert_eq!(json["plexAuthToken"], "my-plex-token");
        // Ensure no unexpected fields
        assert_eq!(json.as_object().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn add_sonarr_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/sonarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1,
                "serverName": "Sonarr1",
                "url": "http://sonarr:8989"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client
            .add_sonarr("Sonarr1", "http://sonarr:8989", "sonarr-key")
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.id, Some(1));
        assert_eq!(response.name, "Sonarr1");
    }

    #[tokio::test]
    async fn add_sonarr_returns_error_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/sonarr"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid URL"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client
            .add_sonarr("Sonarr1", "invalid", "key")
            .await
            .unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn list_sonarr_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings/sonarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1,
                    "serverName": "Sonarr1",
                    "url": "http://sonarr:8989"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client.list_sonarr().await;

        assert!(result.is_ok());
        let servers = result.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "Sonarr1");
    }

    #[tokio::test]
    async fn add_radarr_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/radarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1,
                "serverName": "Radarr1",
                "url": "http://radarr:7878"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client
            .add_radarr("Radarr1", "http://radarr:7878", "radarr-key")
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.id, Some(1));
        assert_eq!(response.name, "Radarr1");
    }

    #[tokio::test]
    async fn list_radarr_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings/radarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1,
                    "serverName": "Radarr1",
                    "url": "http://radarr:7878"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client.list_radarr().await;

        assert!(result.is_ok());
        let servers = result.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "Radarr1");
    }

    #[tokio::test]
    async fn set_overseerr_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/overseerr"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client
            .set_overseerr("http://overseerr:5055", "overseerr-key")
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_overseerr_returns_error_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/overseerr"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid URL"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.set_overseerr("invalid", "key").await.unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn set_tautulli_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/tautulli"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client
            .set_tautulli("http://tautulli:8181", "tautulli-key")
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_tautulli_returns_error_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/tautulli"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid URL"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.set_tautulli("invalid", "key").await.unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn set_plex_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client.set_plex("plex.example.com", 32400, "my-plex-token").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_plex_returns_error_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid Plex config"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.set_plex("invalid", 0, "token").await.unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn plex_set_request_camel_case_stable(hostname in ".*", port in 0u16..=65535u16, token in ".*") {
            let req = PlexSetRequest { plex_hostname: &hostname, plex_port: port, plex_auth_token: &token };
            let json = serde_json::to_value(&req).unwrap();
            prop_assert_eq!(json.get("plexHostname").and_then(|v| v.as_str()), Some(hostname.as_str()));
            prop_assert_eq!(json.get("plexAuthToken").and_then(|v| v.as_str()), Some(token.as_str()));
            prop_assert!(json.get("plex_hostname").is_none(), "snake_case leaked");
            prop_assert!(json.get("plex_auth_token").is_none(), "snake_case leaked");
        }
    }
}
