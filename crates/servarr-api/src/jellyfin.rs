use serde::{Deserialize, Serialize};

use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Client for the Jellyfin API.
///
/// Jellyfin exposes a `GET /health` endpoint that returns `"Healthy"`
/// (text/plain, HTTP 200) when the server is running. No API key required.
#[derive(Debug, Clone)]
pub struct JellyfinClient {
    http: HttpClient,
}

/// Jellyfin user record (minimal fields needed for credential management).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JellyfinUser {
    pub id: String,
    pub name: String,
}

/// Jellyfin auth response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AuthResponse {
    access_token: String,
}

/// Jellyfin set-password request body.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct SetPasswordRequest<'a> {
    current_pw: &'a str,
    new_pw: &'a str,
    reset_password: bool,
}

/// Jellyfin authenticate request body.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct AuthenticateRequest<'a> {
    username: &'a str,
    pw: &'a str,
}

/// Jellyfin startup user request body.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StartupUserRequest<'a> {
    name: &'a str,
    password: &'a str,
}

/// Jellyfin startup configuration request body.
///
/// Sent to `POST /Startup/Configuration` to advance the wizard past step 1.
/// All fields have sensible defaults; we just need to hit the endpoint.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StartupConfigRequest {
    ui_culture: String,
    metadata_country_code: String,
    preferred_metadata_language: String,
}

impl Default for StartupConfigRequest {
    fn default() -> Self {
        Self {
            ui_culture: "en-US".into(),
            metadata_country_code: "US".into(),
            preferred_metadata_language: "en".into(),
        }
    }
}

/// Jellyfin startup remote access request body.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StartupRemoteAccessRequest {
    enable_remote_access: bool,
    enable_automatic_port_mapping: bool,
}

/// Rewraps an `ApiError` with a step-name prefix in the body, preserving the
/// original HTTP status code. Used to distinguish which wizard step failed.
fn wrap_step_err(step: &str, e: ApiError) -> ApiError {
    let status = if let ApiError::ApiResponse { status, .. } = &e {
        *status
    } else {
        500
    };
    ApiError::ApiResponse {
        status,
        body: format!("{step}: {e}"),
    }
}

/// Authorization header value required by the Jellyfin startup wizard endpoints.
const JELLYFIN_AUTH_HEADER: &str = concat!(
    r#"MediaBrowser Client="servarr-operator", "#,
    r#"Device="servarr-operator", "#,
    r#"DeviceId="servarr-operator-device", "#,
    r#"Version="1.0.0""#
);

impl JellyfinClient {
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        Ok(Self {
            http: HttpClient::new(base_url, None)?,
        })
    }

    /// Return `true` if the startup wizard has not been completed yet.
    ///
    /// Calls `GET /Startup/Configuration`; a 200 response means the wizard is pending.
    pub async fn startup_pending(&self) -> Result<bool, ApiError> {
        let url = self.http.base_url().join("/Startup/Configuration")?;
        let resp = self
            .http
            .inner()
            .get(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .send()
            .await
            .map_err(ApiError::Request)?;
        Ok(resp.status().as_u16() == 200)
    }

    /// Advance the startup wizard past step 1 (`POST /Startup/Configuration`).
    ///
    /// Must be called before `startup_set_user`; without this, Jellyfin ≥ 10.9
    /// returns 500 on `POST /Startup/User`.
    async fn startup_configure(&self) -> Result<(), ApiError> {
        let url = self.http.base_url().join("/Startup/Configuration")?;
        let resp = self
            .http
            .inner()
            .post(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .json(&StartupConfigRequest::default())
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

    /// Set the initial admin user via the startup wizard (`POST /Startup/User`).
    pub async fn startup_set_user(&self, username: &str, password: &str) -> Result<(), ApiError> {
        let url = self.http.base_url().join("/Startup/User")?;
        let body = StartupUserRequest {
            name: username,
            password,
        };
        let resp = self
            .http
            .inner()
            .post(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .json(&body)
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

    /// Read the initial-user config via `GET /Startup/FirstUser`.
    ///
    /// Required by some Jellyfin versions (≥ 10.9) to advance the wizard state
    /// past the user-creation step before calling `POST /Startup/User`.
    /// Returns `Ok(())` on 200/204 and on 404 (endpoint not present in older
    /// versions); returns `Err` only for unexpected status codes.
    async fn startup_get_first_user(&self) -> Result<(), ApiError> {
        let url = self.http.base_url().join("/Startup/FirstUser")?;
        let resp = self
            .http
            .inner()
            .get(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .send()
            .await
            .map_err(ApiError::Request)?;
        let status = resp.status().as_u16();
        if resp.status().is_success() || status == 404 {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Configure remote access as part of the startup wizard (`POST /Startup/RemoteAccess`).
    async fn startup_set_remote_access(&self) -> Result<(), ApiError> {
        let url = self.http.base_url().join("/Startup/RemoteAccess")?;
        let body = StartupRemoteAccessRequest {
            enable_remote_access: true,
            enable_automatic_port_mapping: false,
        };
        let resp = self
            .http
            .inner()
            .post(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .json(&body)
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

    /// Complete the startup wizard (`POST /Startup/Complete`).
    ///
    /// Without this call the admin user cannot authenticate via the regular API.
    async fn startup_complete(&self) -> Result<(), ApiError> {
        let url = self.http.base_url().join("/Startup/Complete")?;
        let resp = self
            .http
            .inner()
            .post(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
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

    /// Authenticate as a user and return the access token.
    pub async fn authenticate(&self, username: &str, password: &str) -> Result<String, ApiError> {
        let url = self.http.base_url().join("/Users/AuthenticateByName")?;
        let body = AuthenticateRequest {
            username,
            pw: password,
        };
        let resp = self
            .http
            .inner()
            .post(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;
        if resp.status().is_success() {
            let auth: AuthResponse = resp.json().await.map_err(ApiError::Request)?;
            Ok(auth.access_token)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// List all users (requires an authenticated token).
    pub async fn list_users(&self, token: &str) -> Result<Vec<JellyfinUser>, ApiError> {
        let url = self.http.base_url().join("/Users")?;
        let resp = self
            .http
            .inner()
            .get(url)
            .header(
                "X-Emby-Authorization",
                format!("{JELLYFIN_AUTH_HEADER}, Token=\"{token}\""),
            )
            .send()
            .await
            .map_err(ApiError::Request)?;
        if resp.status().is_success() {
            resp.json::<Vec<JellyfinUser>>()
                .await
                .map_err(ApiError::Request)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Change a user's password (`POST /Users/{userId}/Password`).
    pub async fn set_password(
        &self,
        token: &str,
        user_id: &str,
        new_password: &str,
    ) -> Result<(), ApiError> {
        let url = self
            .http
            .base_url()
            .join(&format!("/Users/{user_id}/Password"))?;
        let body = SetPasswordRequest {
            current_pw: "",
            new_pw: new_password,
            reset_password: true,
        };
        let resp = self
            .http
            .inner()
            .post(url)
            .header(
                "X-Emby-Authorization",
                format!("{JELLYFIN_AUTH_HEADER}, Token=\"{token}\""),
            )
            .json(&body)
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

    /// Configure the Jellyfin admin account.
    ///
    /// If the startup wizard is pending, runs the full wizard sequence.
    /// Otherwise, authenticates as the existing admin and changes the password.
    ///
    /// Each wizard step is wrapped with its name so failures are identifiable
    /// in operator logs (e.g. `startup_configure: API returned 500: …`).
    pub async fn configure_admin(&self, username: &str, password: &str) -> Result<(), ApiError> {
        if self.startup_pending().await? {
            // Step 1: set locale/language (required before user creation in ≥ 10.9).
            self.startup_configure()
                .await
                .map_err(|e| wrap_step_err("startup_configure", e))?;
            // Step 2: GET /Startup/FirstUser advances the wizard state machine in
            // some Jellyfin versions. Non-fatal if the endpoint returns 404.
            self.startup_get_first_user()
                .await
                .map_err(|e| wrap_step_err("startup_get_first_user", e))?;
            // Step 3: create the initial admin user.
            self.startup_set_user(username, password)
                .await
                .map_err(|e| wrap_step_err("startup_set_user", e))?;
            // Step 4: enable remote access.
            self.startup_set_remote_access()
                .await
                .map_err(|e| wrap_step_err("startup_set_remote_access", e))?;
            // Step 5: complete the wizard.
            return self
                .startup_complete()
                .await
                .map_err(|e| wrap_step_err("startup_complete", e));
        }

        // Wizard complete: authenticate as the admin user and update the password.
        let token = self.authenticate(username, password).await?;
        let users = self.list_users(&token).await?;
        let admin = users
            .into_iter()
            .find(|u| u.name.eq_ignore_ascii_case(username))
            .ok_or_else(|| ApiError::ApiResponse {
                status: 404,
                body: format!("user '{username}' not found in Jellyfin"),
            })?;
        self.set_password(&token, &admin.id, password).await
    }
}

impl HealthCheck for JellyfinClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let url = self.http.base_url().join("/health")?;
        let resp = self.http.inner().get(url).send().await?;
        Ok(resp.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn client(server: &MockServer) -> JellyfinClient {
        JellyfinClient::new(&server.uri()).expect("client")
    }

    #[test]
    fn new_constructs() {
        JellyfinClient::new("http://localhost:8096").unwrap();
    }

    #[tokio::test]
    async fn is_healthy_returns_true_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Healthy"))
            .mount(&server)
            .await;
        assert!(client(&server).is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn is_healthy_returns_false_on_503() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        assert!(!client(&server).is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn startup_pending_returns_true_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Startup/Configuration"))
            .and(header_exists("X-Emby-Authorization"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        assert!(client(&server).startup_pending().await.unwrap());
    }

    #[tokio::test]
    async fn startup_pending_returns_false_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Startup/Configuration"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(!client(&server).startup_pending().await.unwrap());
    }

    #[tokio::test]
    async fn startup_set_user_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Startup/User"))
            .and(header_exists("X-Emby-Authorization"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        client(&server)
            .startup_set_user("admin", "pass")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn startup_set_user_returns_error_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Startup/User"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;
        let err = client(&server)
            .startup_set_user("admin", "pass")
            .await
            .unwrap_err();
        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("unexpected: {other}"),
        }
    }

    #[tokio::test]
    async fn authenticate_returns_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateByName"))
            .and(header_exists("X-Emby-Authorization"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"AccessToken": "tok-abc123"})),
            )
            .mount(&server)
            .await;
        let token = client(&server).authenticate("admin", "pass").await.unwrap();
        assert_eq!(token, "tok-abc123");
    }

    #[tokio::test]
    async fn authenticate_returns_error_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateByName"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;
        let err = client(&server)
            .authenticate("admin", "wrong")
            .await
            .unwrap_err();
        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 401),
            other => panic!("unexpected: {other}"),
        }
    }

    #[tokio::test]
    async fn list_users_returns_user_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Users"))
            .and(header_exists("X-Emby-Authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"Id": "user-1", "Name": "Admin"},
                {"Id": "user-2", "Name": "Guest"},
            ])))
            .mount(&server)
            .await;
        let users = client(&server).list_users("my-token").await.unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].id, "user-1");
        assert_eq!(users[0].name, "Admin");
    }

    #[tokio::test]
    async fn set_password_calls_correct_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Users/user-1/Password"))
            .and(header_exists("X-Emby-Authorization"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        client(&server)
            .set_password("tok", "user-1", "newpass")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn configure_admin_uses_startup_wizard_when_pending() {
        let server = MockServer::start().await;
        // startup_pending → 200 (wizard active)
        Mock::given(method("GET"))
            .and(path("/Startup/Configuration"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        // startup_configure (POST /Startup/Configuration)
        Mock::given(method("POST"))
            .and(path("/Startup/Configuration"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        // startup_get_first_user (GET /Startup/FirstUser)
        Mock::given(method("GET"))
            .and(path("/Startup/FirstUser"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        // startup_set_user
        Mock::given(method("POST"))
            .and(path("/Startup/User"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        // startup_set_remote_access
        Mock::given(method("POST"))
            .and(path("/Startup/RemoteAccess"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        // startup_complete
        Mock::given(method("POST"))
            .and(path("/Startup/Complete"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        client(&server)
            .configure_admin("admin", "pass")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn configure_admin_changes_password_when_wizard_complete() {
        let server = MockServer::start().await;
        // startup_pending → 404 (wizard done)
        Mock::given(method("GET"))
            .and(path("/Startup/Configuration"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        // authenticate
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateByName"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"AccessToken": "tok-xyz"})),
            )
            .mount(&server)
            .await;
        // list_users
        Mock::given(method("GET"))
            .and(path("/Users"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"Id": "u1", "Name": "admin"}])),
            )
            .mount(&server)
            .await;
        // set_password
        Mock::given(method("POST"))
            .and(path("/Users/u1/Password"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        client(&server)
            .configure_admin("admin", "newpass")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn configure_admin_returns_error_when_user_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Startup/Configuration"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateByName"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"AccessToken": "tok"})),
            )
            .mount(&server)
            .await;
        // list_users returns a different user
        Mock::given(method("GET"))
            .and(path("/Users"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"Id": "u99", "Name": "someone_else"}])),
            )
            .mount(&server)
            .await;
        let err = client(&server)
            .configure_admin("admin", "pass")
            .await
            .unwrap_err();
        match err {
            ApiError::ApiResponse { status: 404, .. } => {}
            other => panic!("expected 404, got: {other}"),
        }
    }
}
