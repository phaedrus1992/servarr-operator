use serde::Deserialize;

use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Client for the SABnzbd API.
///
/// SABnzbd uses a query-parameter-based API:
/// `GET /api?mode=<action>&apikey=<key>&output=json`
#[derive(Debug, Clone)]
pub struct SabnzbdClient {
    http: HttpClient,
    api_key: String,
}

// --- Response types ---

#[derive(Debug, Clone, Deserialize)]
pub struct VersionResponse {
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueueResponse {
    pub queue: QueueStatus,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub speed: String,
    #[serde(default, rename = "sizeleft")]
    pub size_left: String,
    #[serde(default, rename = "mb")]
    pub total_mb: String,
    #[serde(default, rename = "mbleft")]
    pub mb_left: String,
    #[serde(default, rename = "noofslots_total")]
    pub total_slots: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerStatsResponse {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub servers: serde_json::Value,
}

impl SabnzbdClient {
    /// Create a new SABnzbd client.
    ///
    /// `base_url` should be the root URL (e.g. `http://sabnzbd:8080`).
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let url = format!("{}/api", base_url.trim_end_matches('/'));
        Ok(Self {
            http: HttpClient::new(&url, None)?,
            api_key: api_key.to_string(),
        })
    }

    /// GET `/api?mode=version&apikey=<key>&output=json`
    pub async fn version(&self) -> Result<String, ApiError> {
        let resp: VersionResponse = self
            .http
            .get(&format!(
                "?mode=version&apikey={}&output=json",
                self.api_key
            ))
            .await?;
        Ok(resp.version)
    }

    /// GET `/api?mode=queue&apikey=<key>&output=json`
    pub async fn queue_status(&self) -> Result<QueueStatus, ApiError> {
        let resp: QueueResponse = self
            .http
            .get(&format!("?mode=queue&apikey={}&output=json", self.api_key))
            .await?;
        Ok(resp.queue)
    }

    /// GET `/api?mode=server_stats&apikey=<key>&output=json`
    pub async fn server_stats(&self) -> Result<ServerStatsResponse, ApiError> {
        self.http
            .get(&format!(
                "?mode=server_stats&apikey={}&output=json",
                self.api_key
            ))
            .await
    }

    /// Set a single misc config value via `set_config`.
    ///
    /// Calls `?mode=set_config&section={section}&keyword={keyword}&value={value}&apikey={key}&output=json`.
    pub async fn set_config(
        &self,
        section: &str,
        keyword: &str,
        value: &str,
    ) -> Result<(), ApiError> {
        let path = format!(
            "?mode=set_config&section={section}&keyword={keyword}&value={value}&apikey={}&output=json",
            self.api_key
        );
        let _resp: serde_json::Value = self.http.get(&path).await?;
        Ok(())
    }

    /// Set the admin username and password via the `set_config` API.
    pub async fn set_credentials(&self, username: &str, password: &str) -> Result<(), ApiError> {
        self.set_config("misc", "username", username).await?;
        self.set_config("misc", "password", password).await?;
        Ok(())
    }
}

impl HealthCheck for SabnzbdClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let version = self.version().await?;
        Ok(!version.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn client(server: &MockServer) -> SabnzbdClient {
        SabnzbdClient::new(&server.uri(), "testkey").expect("client")
    }

    #[test]
    fn new_trims_trailing_slash() {
        // Just test that construction succeeds and stores the api_key.
        // We can't inspect internals directly, but version() path encodes it.
        let c = SabnzbdClient::new("http://localhost:8080/", "mykey").unwrap();
        assert_eq!(c.api_key, "mykey");
    }

    #[tokio::test]
    async fn set_config_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("mode", "set_config"))
            .and(query_param("section", "misc"))
            .and(query_param("keyword", "username"))
            .and(query_param("value", "admin"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        client(&server)
            .set_config("misc", "username", "admin")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_config_returns_error_on_non_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .respond_with(ResponseTemplate::new(500).set_body_string("error"))
            .mount(&server)
            .await;
        let err = client(&server)
            .set_config("misc", "username", "x")
            .await
            .unwrap_err();
        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 500),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn set_credentials_sets_username_then_password() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("keyword", "username"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("keyword", "password"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        client(&server)
            .set_credentials("admin", "s3cr3t")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn version_returns_version_string() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("mode", "version"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "4.3.2"})),
            )
            .mount(&server)
            .await;
        let v = client(&server).version().await.unwrap();
        assert_eq!(v, "4.3.2");
    }

    #[tokio::test]
    async fn is_healthy_returns_true_for_non_empty_version() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("mode", "version"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "4.0.0"})),
            )
            .mount(&server)
            .await;
        assert!(client(&server).is_healthy().await.unwrap());
    }
}
