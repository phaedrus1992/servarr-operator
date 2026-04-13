use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Client for the Tautulli API.
///
/// Tautulli uses a query-parameter-based API:
/// `GET /api/v2?cmd=<action>&...`
#[derive(Debug, Clone)]
pub struct TautulliClient {
    http: HttpClient,
}

impl TautulliClient {
    /// Create a new Tautulli client.
    ///
    /// `base_url` should be the root URL (e.g. `http://tautulli:8181`).
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        let url = format!("{}/api/v2", base_url.trim_end_matches('/'));
        Ok(Self {
            http: HttpClient::new(&url, None)?,
        })
    }

    /// Set admin credentials via the `set_credentials` command.
    ///
    /// Calls `GET /api/v2?cmd=set_credentials&username=...&password=...`.
    pub async fn set_credentials(&self, username: &str, password: &str) -> Result<(), ApiError> {
        let mut url = self.http.base_url().clone();
        url.query_pairs_mut()
            .append_pair("cmd", "set_credentials")
            .append_pair("username", username)
            .append_pair("password", password);

        let resp = self
            .http
            .inner()
            .get(url)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }
}

impl HealthCheck for TautulliClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let mut url = self.http.base_url().clone();
        url.query_pairs_mut()
            .append_pair("cmd", "status")
            .append_pair("output", "json");

        let resp = self
            .http
            .inner()
            .get(url)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if !resp.status().is_success() {
            return Ok(false);
        }
        let body: serde_json::Value = resp.json().await.map_err(ApiError::Request)?;
        // Tautulli returns {"response": {"result": "success", ...}}
        Ok(body
            .get("response")
            .and_then(|r| r.get("result"))
            .and_then(|r| r.as_str())
            == Some("success"))
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn client(server: &MockServer) -> TautulliClient {
        TautulliClient::new(&server.uri()).expect("client")
    }

    #[test]
    fn new_trims_trailing_slash() {
        TautulliClient::new("http://localhost:8181/").unwrap();
    }

    #[tokio::test]
    async fn set_credentials_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2"))
            .and(query_param("cmd", "set_credentials"))
            .and(query_param("username", "admin"))
            .and(query_param("password", "s3cr3t"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        client(&server)
            .set_credentials("admin", "s3cr3t")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_credentials_returns_error_on_non_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
            .mount(&server)
            .await;
        let err = client(&server)
            .set_credentials("admin", "bad")
            .await
            .unwrap_err();
        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 403),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn is_healthy_returns_true_on_success_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2"))
            .and(query_param("cmd", "status"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"response": {"result": "success"}})),
            )
            .mount(&server)
            .await;
        assert!(client(&server).is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn is_healthy_returns_false_on_non_success_result() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"response": {"result": "error"}})),
            )
            .mount(&server)
            .await;
        assert!(!client(&server).is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn is_healthy_returns_false_on_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        assert!(!client(&server).is_healthy().await.unwrap());
    }
}
