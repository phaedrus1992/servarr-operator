use serde::de::DeserializeOwned;
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

/// Request body for setting Plex token. Maintainerr's `/api/settings/plex/token`
/// handler reads the `plex_auth_token` field (#156).
#[derive(Serialize)]
struct PlexTokenSetRequest<'a> {
    plex_auth_token: &'a str,
}

/// Request body for setting Plex hostname and port.
#[derive(Serialize)]
struct PlexSettingsRequest<'a> {
    plex_hostname: &'a str,
    plex_port: u16,
}

/// Maintainerr's status envelope for mutating endpoints. Returned with HTTP 200
/// even when the operation was rejected (`status == "NOK"`), so the body must be
/// inspected rather than trusting the HTTP status alone (#156).
#[derive(Deserialize)]
struct StatusEnvelope {
    status: String,
    message: Option<String>,
}

/// Classify an HTTP-success response body: a `{ status: "NOK" }` envelope (case
/// insensitive) maps to [`ApiError::OperationFailed`]; anything else (including
/// non-JSON or non-envelope bodies) passes through as success (#156).
fn classify_success_body(body: String) -> Result<String, ApiError> {
    if let Ok(envelope) = serde_json::from_str::<StatusEnvelope>(&body)
        && envelope.status.eq_ignore_ascii_case("NOK")
    {
        return Err(ApiError::OperationFailed {
            message: envelope
                .message
                .unwrap_or_else(|| "Maintainerr reported failure".to_string()),
        });
    }
    Ok(body)
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
        Self::check_envelope_json(resp).await
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
        Self::check_envelope_json(resp).await
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
        Self::check_envelope(resp).await
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
        Self::check_envelope(resp).await
    }

    // ===== Plex Methods =====

    /// Set Plex authentication token.
    ///
    /// Must be called before [`set_plex`](Self::set_plex): Maintainerr refuses to
    /// save Plex server settings until an auth token is present (#156).
    pub async fn set_plex_token(&self, token: &str) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings/plex/token", self.base_url);
        let body = PlexTokenSetRequest {
            plex_auth_token: token,
        };
        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;
        Self::check_envelope(resp).await
    }

    /// Set Plex hostname and port. Requires an auth token to already be saved via
    /// [`set_plex_token`](Self::set_plex_token) (#156).
    pub async fn set_plex(&self, hostname: &str, port: u16) -> Result<(), ApiError> {
        let endpoint = format!("{}/api/settings", self.base_url);
        let body = PlexSettingsRequest {
            plex_hostname: hostname,
            plex_port: port,
        };
        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;
        Self::check_envelope(resp).await
    }

    /// Read a Maintainerr mutating response into its body, mapping failures to errors.
    /// A non-2xx status maps to [`ApiError::ApiResponse`]; a 2xx carrying a
    /// `{ status: "NOK" }` envelope maps to [`ApiError::OperationFailed`] so silent
    /// failures aren't recorded (#156). Otherwise the raw body is returned for the
    /// caller to interpret (empty or non-envelope bodies are treated as success).
    async fn envelope_body(resp: reqwest::Response) -> Result<String, ApiError> {
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Maintainerr error response body");
                String::new()
            });
            return Err(ApiError::ApiResponse { status, body });
        }

        let body = resp.text().await.unwrap_or_else(|e| {
            tracing::debug!(error = %e, "failed to read Maintainerr response body");
            String::new()
        });
        classify_success_body(body)
    }

    /// Validate a Maintainerr response from an endpoint that returns no body of
    /// interest on success (the setters). See [`envelope_body`](Self::envelope_body).
    async fn check_envelope(resp: reqwest::Response) -> Result<(), ApiError> {
        Self::envelope_body(resp).await.map(|_| ())
    }

    /// Validate a Maintainerr response from an endpoint that returns a JSON object on
    /// success (the `add_*` endpoints), inspecting the envelope before deserializing
    /// the success type `T`. See [`envelope_body`](Self::envelope_body).
    async fn check_envelope_json<T: DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, ApiError> {
        let body = Self::envelope_body(resp).await?;
        serde_json::from_str::<T>(&body).map_err(|e| ApiError::ApiResponse {
            status: 200,
            body: format!("failed to decode Maintainerr response: {e}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    proptest! {
        // classify_success_body must never panic on arbitrary input, and must
        // never mistake a non-envelope body (including invalid JSON, or JSON
        // that isn't a status envelope) for a failure.
        #[test]
        fn classify_success_body_never_panics_on_arbitrary_input(body in ".*") {
            let _ = classify_success_body(body);
        }

        // Any status value that is a case-insensitive match for "NOK" must be
        // classified as OperationFailed, regardless of casing (#156).
        #[test]
        fn classify_success_body_detects_nok_case_insensitively(
            case in prop::sample::select(vec!["NOK", "nok", "Nok", "nOk", "NoK"]),
            message in proptest::option::of(".*"),
        ) {
            let body = serde_json::json!({ "status": case, "message": message }).to_string();
            let result = classify_success_body(body);
            let is_operation_failed = matches!(result, Err(ApiError::OperationFailed { .. }));
            prop_assert!(is_operation_failed);
        }

        // Any status value that is not "NOK" (case-insensitively) must pass
        // through as success, carrying the original body unchanged.
        #[test]
        fn classify_success_body_passes_through_non_nok_status(
            status in "[a-zA-Z]{1,10}".prop_filter("not NOK", |s| !s.eq_ignore_ascii_case("nok")),
        ) {
            let body = serde_json::json!({ "status": status }).to_string();
            let result = classify_success_body(body.clone());
            prop_assert_eq!(result.ok(), Some(body));
        }

        // ServerResponse must round-trip through serialize -> deserialize for
        // arbitrary field values, and must always serialize `name` back out
        // under the `serverName` key, never the `name` alias.
        #[test]
        fn server_response_roundtrips_and_serializes_servername(
            id in proptest::option::of(any::<i32>()),
            name in ".*",
            url in ".*",
        ) {
            let original = ServerResponse { id, name: name.clone(), url: url.clone() };
            let json = serde_json::to_string(&original).expect("should serialize");
            prop_assert!(json.contains("\"serverName\":"), "got: {json}");
            prop_assert!(!json.contains("\"name\":"), "alias leaked into output: {json}");

            let parsed: ServerResponse = serde_json::from_str(&json).expect("should deserialize");
            prop_assert_eq!(parsed.id, id);
            prop_assert_eq!(parsed.name, name);
            prop_assert_eq!(parsed.url, url);
        }
    }

    #[test]
    fn classify_success_body_non_json_is_success() {
        let result = classify_success_body("not json at all".to_string());
        assert_eq!(result.ok(), Some("not json at all".to_string()));
    }

    #[test]
    fn classify_success_body_empty_is_success() {
        let result = classify_success_body(String::new());
        assert_eq!(result.ok(), Some(String::new()));
    }

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
    async fn add_sonarr_rejects_nok_envelope() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The add_* endpoints return HTTP 200 with a NOK envelope on failure rather
        // than the created server object; that must surface as OperationFailed, not a
        // decode error or a silent success (#156).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/sonarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "NOK",
                "code": 0,
                "message": "Sonarr URL is not reachable"
            })))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client
            .add_sonarr("Sonarr1", "http://sonarr:8989", "key")
            .await
            .unwrap_err();

        match err {
            ApiError::OperationFailed { message } => {
                assert!(message.contains("not reachable"), "got: {message}");
            }
            other => panic!("expected OperationFailed, got: {other}"),
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
    async fn set_plex_token_calls_correct_endpoint() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Maintainerr's POST /api/settings/plex/token reads the `plex_auth_token`
        // field; matching on it guards against a regression to `access_token` (#156).
        Mock::given(method("POST"))
            .and(path("/api/settings/plex/token"))
            .and(body_partial_json(
                serde_json::json!({ "plex_auth_token": "plex-auth-token" }),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let result = client.set_plex_token("plex-auth-token").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_plex_rejects_nok_envelope() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Maintainerr returns HTTP 200 with a `{ status: "NOK" }` envelope on failure
        // (e.g. saving Plex server settings before authenticating). A 200 must not be
        // treated as success when the envelope reports NOK (#156).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "NOK",
                "code": 0,
                "message": "Authenticate with Plex before saving Plex server settings."
            })))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.set_plex("plex-hostname", 32400).await.unwrap_err();

        match err {
            ApiError::OperationFailed { message } => {
                assert!(message.contains("Authenticate with Plex"), "got: {message}");
            }
            other => panic!("expected OperationFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn set_plex_token_returns_error_on_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/plex/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid token"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.set_plex_token("invalid").await.unwrap_err();

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
        let result = client.set_plex("plex-hostname", 32400).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_plex_returns_error_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid hostname"))
            .mount(&server)
            .await;

        let client = MaintainerrClient::new(&server.uri(), "test-key").expect("should construct");
        let err = client.set_plex("invalid", 32400).await.unwrap_err();

        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }
}
