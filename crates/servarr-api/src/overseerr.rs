use serde::Serialize;

use crate::client::ApiError;

/// Client for the Overseerr settings API.
///
/// Wraps the `overseerr` crate to manage Sonarr/Radarr server registrations
/// in Overseerr for media request routing.
pub struct OverseerrClient {
    config: overseerr::apis::configuration::Configuration,
}

/// Request body for `PUT /api/v1/auth/local`.
#[derive(Serialize)]
struct LocalAuthRequest<'a> {
    username: &'a str,
    password: &'a str,
}

fn map_err<E: std::fmt::Debug>(e: overseerr::apis::Error<E>) -> ApiError {
    ApiError::ApiResponse {
        status: 0,
        body: format!("{e:?}"),
    }
}

impl OverseerrClient {
    /// Create a new Overseerr API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://overseerr:5055`).
    /// `api_key` is sent as the `X-Api-Key` header.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        let mut config = overseerr::apis::configuration::Configuration::new();
        config.base_path = base_url.trim_end_matches('/').to_string();
        config.api_key = Some(overseerr::apis::configuration::ApiKey {
            prefix: None,
            key: api_key.to_string(),
        });
        Self { config }
    }

    /// List all Sonarr server registrations.
    pub async fn list_sonarr(&self) -> Result<Vec<overseerr::models::SonarrSettings>, ApiError> {
        overseerr::apis::settings_api::list_sonarr(&self.config)
            .await
            .map_err(map_err)
    }

    /// Register a new Sonarr server.
    pub async fn create_sonarr(
        &self,
        settings: overseerr::models::SonarrSettings,
    ) -> Result<overseerr::models::SonarrSettings, ApiError> {
        overseerr::apis::settings_api::create_sonarr(&self.config, settings)
            .await
            .map_err(map_err)
    }

    /// Update an existing Sonarr server registration.
    pub async fn update_sonarr(
        &self,
        id: i32,
        settings: overseerr::models::SonarrSettings,
    ) -> Result<overseerr::models::SonarrSettings, ApiError> {
        overseerr::apis::settings_api::update_sonarr(&self.config, id, settings)
            .await
            .map_err(map_err)
    }

    /// Remove a Sonarr server registration.
    pub async fn delete_sonarr(&self, id: i32) -> Result<(), ApiError> {
        overseerr::apis::settings_api::delete_sonarr(&self.config, id)
            .await
            .map_err(map_err)
            .map(|_| ())
    }

    /// List all Radarr server registrations.
    pub async fn list_radarr(&self) -> Result<Vec<overseerr::models::RadarrSettings>, ApiError> {
        overseerr::apis::settings_api::list_radarr(&self.config)
            .await
            .map_err(map_err)
    }

    /// Register a new Radarr server.
    pub async fn create_radarr(
        &self,
        settings: overseerr::models::RadarrSettings,
    ) -> Result<overseerr::models::RadarrSettings, ApiError> {
        overseerr::apis::settings_api::create_radarr(&self.config, settings)
            .await
            .map_err(map_err)
    }

    /// Update an existing Radarr server registration.
    pub async fn update_radarr(
        &self,
        id: i32,
        settings: overseerr::models::RadarrSettings,
    ) -> Result<overseerr::models::RadarrSettings, ApiError> {
        overseerr::apis::settings_api::update_radarr(&self.config, id, settings)
            .await
            .map_err(map_err)
    }

    /// Remove a Radarr server registration.
    pub async fn delete_radarr(&self, id: i32) -> Result<(), ApiError> {
        overseerr::apis::settings_api::delete_radarr(&self.config, id)
            .await
            .map_err(map_err)
            .map(|_| ())
    }

    /// Configure local authentication via `PUT /api/v1/auth/local`.
    ///
    /// Sets the admin username and password for Overseerr's local auth provider.
    pub async fn setup_local_auth(&self, username: &str, password: &str) -> Result<(), ApiError> {
        let url = format!("{}/api/v1/auth/local", self.config.base_path);
        let body = LocalAuthRequest { username, password };
        let resp = self
            .config
            .client
            .put(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::ApiResponse {
                status: 0,
                body: e.to_string(),
            })?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overseerr_client_new_constructs() {
        let client = OverseerrClient::new("http://localhost:5055", "test-key");
        assert_eq!(client.config.base_path, "http://localhost:5055");
        assert!(client.config.api_key.is_some());
        let api_key = client.config.api_key.as_ref().unwrap();
        assert_eq!(api_key.key, "test-key");
        assert!(api_key.prefix.is_none());
    }

    #[test]
    fn overseerr_client_new_trims_trailing_slash() {
        let client = OverseerrClient::new("http://localhost:5055/", "key");
        assert_eq!(client.config.base_path, "http://localhost:5055");
    }

    #[tokio::test]
    async fn setup_local_auth_calls_correct_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/auth/local"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = OverseerrClient::new(&server.uri(), "test-key");
        client.setup_local_auth("admin", "pass").await.unwrap();
    }

    #[tokio::test]
    async fn setup_local_auth_returns_error_on_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/auth/local"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;
        let client = OverseerrClient::new(&server.uri(), "test-key");
        let err = client.setup_local_auth("admin", "wrong").await.unwrap_err();
        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 401),
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn map_err_formats_debug() {
        // The overseerr SDK Error type wraps a reqwest error or a response body.
        // We can test map_err by constructing an ApiError from a simple Debug value.
        let sdk_err: overseerr::apis::Error<()> =
            overseerr::apis::Error::Io(std::io::Error::other("test io error"));
        let api_err = map_err(sdk_err);
        match api_err {
            ApiError::ApiResponse { status, body } => {
                assert_eq!(status, 0);
                assert!(
                    body.contains("test io error"),
                    "body should contain the error message, got: {body}"
                );
            }
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }
}
