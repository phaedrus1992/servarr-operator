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

/// Request body for setting Plex token.
#[derive(Serialize)]
struct PlexTokenRequest<'a> {
    token: &'a str,
}

/// Request body for setting Plex with hostname, port, and optional token.
#[derive(Serialize)]
struct PlexSetRequest<'a> {
    hostname: &'a str,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<&'a str>,
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
            "X-Api-Key",
            reqwest::header::HeaderValue::from_str(api_key)
                .map_err(|_| ApiError::InvalidApiKey)?,
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ApiError::Request(e))?;

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
        let body = SonarrAddRequest {
            name,
            url,
            api_key,
        };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            resp.json()
                .await
                .map_err(|e| ApiError::Request(e))
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Update an existing Sonarr server.
    pub async fn update_sonarr(
        &self,
        id: i32,
        settings: ServerResponse,
    ) -> Result<ServerResponse, ApiError> {
        let endpoint = format!("{}/api/settings/sonarr/{}", self.base_url, id);

        let resp = self
            .client
            .put(&endpoint)
            .json(&settings)
            .send()
            .await
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            resp.json()
                .await
                .map_err(|e| ApiError::Request(e))
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
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
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            resp.json()
                .await
                .map_err(|e| ApiError::Request(e))
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Delete a Sonarr server by ID.
    pub async fn delete_sonarr(&self, id: i32) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings/sonarr/{}", self.base_url, id);

        let resp = self
            .client
            .delete(&endpoint)
            .send()
            .await
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
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
        let body = RadarrAddRequest {
            name,
            url,
            api_key,
        };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            resp.json()
                .await
                .map_err(|e| ApiError::Request(e))
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Update an existing Radarr server.
    pub async fn update_radarr(
        &self,
        id: i32,
        settings: ServerResponse,
    ) -> Result<ServerResponse, ApiError> {
        let endpoint = format!("{}/api/settings/radarr/{}", self.base_url, id);

        let resp = self
            .client
            .put(&endpoint)
            .json(&settings)
            .send()
            .await
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            resp.json()
                .await
                .map_err(|e| ApiError::Request(e))
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
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
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            resp.json()
                .await
                .map_err(|e| ApiError::Request(e))
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Delete a Radarr server by ID.
    pub async fn delete_radarr(&self, id: i32) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings/radarr/{}", self.base_url, id);

        let resp = self
            .client
            .delete(&endpoint)
            .send()
            .await
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
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
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
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
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
            Err(ApiError::ApiResponse { status, body })
        }
    }

    // ===== Plex Methods =====

    /// Set Plex token.
    pub async fn set_plex_token(&self, token: &str) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings/plex/token", self.base_url);
        let body = PlexTokenRequest { token };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Set Plex with hostname, port, and optional token.
    pub async fn set_plex(
        &self,
        hostname: &str,
        port: u16,
        token: Option<&str>,
    ) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings", self.base_url);
        let body = PlexSetRequest {
            hostname,
            port,
            token,
        };

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::Request(e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
            Err(ApiError::ApiResponse { status, body })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintainerr_client_new_constructs() {
        let client = MaintainerrClient::new("http://localhost:6246", "test-key")
            .expect("should construct");
        assert_eq!(client.base_url, "http://localhost:6246");
    }

    #[test]
    fn maintainerr_client_new_trims_trailing_slash() {
        let client = MaintainerrClient::new("http://localhost:6246/", "test-key")
            .expect("should construct");
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

    #[tokio::test]
    async fn add_sonarr_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
    async fn delete_sonarr_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/settings/sonarr/1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client.delete_sonarr(1).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_sonarr_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/settings/sonarr/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1,
                "serverName": "Sonarr1Updated",
                "url": "http://sonarr2:8989"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let settings = ServerResponse {
            id: Some(1),
            name: "Sonarr1Updated".to_string(),
            url: "http://sonarr2:8989".to_string(),
        };
        let result = client.update_sonarr(1, settings).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn add_radarr_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
    async fn delete_radarr_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/settings/radarr/1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client.delete_radarr(1).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_radarr_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/settings/radarr/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1,
                "serverName": "Radarr1Updated",
                "url": "http://radarr2:7878"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let settings = ServerResponse {
            id: Some(1),
            name: "Radarr1Updated".to_string(),
            url: "http://radarr2:7878".to_string(),
        };
        let result = client.update_radarr(1, settings).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_overseerr_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/overseerr"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid URL"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client
            .set_overseerr("invalid", "key")
            .await
            .unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn set_tautulli_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
    async fn set_plex_token_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/plex/token"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client.set_plex_token("plex-token-123").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_plex_token_returns_error_on_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/plex/token"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Invalid token"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.set_plex_token("invalid").await.unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 401),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn set_plex_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client
            .set_plex("localhost", 32400, Some("plex-token"))
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_plex_without_token_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client.set_plex("localhost", 32400, None).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_plex_returns_error_on_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid port"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.set_plex("localhost", 0, None).await.unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }
}
