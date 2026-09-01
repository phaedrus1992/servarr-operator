//! Houndarr API client: session/CSRF form automation (#606).
//!
//! Houndarr has no JSON API for instance registration — only a read-only
//! dashboard widget, logs, status, and a run-now trigger. Confirmed against
//! Houndarr v1.13.2 source (github.com/av1155/houndarr):
//!
//! - `POST /login` (form: `username`, `password`) sets a `houndarr_session`
//!   cookie (`HttpOnly`, signed) and a `houndarr_csrf` cookie (plain,
//!   readable) on success — a 303 redirect. Bad credentials answer 401;
//!   rate-limited answers 429 (both re-render the login page).
//! - Every mutating request (POST/PUT/PATCH/DELETE) must echo the
//!   `houndarr_csrf` cookie's value back via the `X-CSRF-Token` header
//!   (double-submit CSRF; `houndarr.auth.csrf.validate_csrf`). The same
//!   value is also accepted as a `csrf_token` form field for plain HTML
//!   form submissions, but that path makes `validate_csrf` call
//!   `request.form()` itself before the route's own `Form()` params get
//!   parsed, consuming the body stream first -- every other field comes
//!   back "missing" (422) as a result. Always use the header.
//! - `GET /api/status` returns `{"instances": [{"name", "type", ...}, ...]}`
//!   JSON — listing registered instances needs no HTML scraping.
//! - `POST /settings/instances` (form: `name`, `type`, `url`, `api_key`,
//!   plus policy fields that default server-side) is the only way to
//!   register an instance; a 200 (HTMX partial) or 303 both mean success.
//!   Also requires `connection_verified=true` -- without it the server
//!   rejects the create outright with "Test connection successfully before
//!   adding." (`services/instance_submit.py`). The server re-probes the
//!   *arr instance itself either way, so this is just satisfying a UI-state
//!   flag, not skipping real validation.
//!
//! This client tracks the session/CSRF cookies itself rather than via
//! reqwest's cookie jar, so behavior stays deterministic and testable
//! against wiremock. This is explicitly a stopgap (per the design spec)
//! pending #775's investigation into a proper upstream JSON API.

use serde::Deserialize;

use crate::client::ApiError;
use crate::cross_app_sync::{CrossAppSync, RegisteredArrInstance};

const SESSION_COOKIE_NAME: &str = "houndarr_session";
const CSRF_COOKIE_NAME: &str = "houndarr_csrf";
const SUPPORTED_KINDS: &[&str] = &["sonarr", "radarr", "lidarr", "readarr"];

/// Client for Houndarr's session/CSRF-authenticated instance-registration UI.
#[derive(Clone)]
pub struct HoundarrClient {
    base_url: String,
    client: reqwest::Client,
    cookie_header: String,
    csrf_token: String,
}

impl std::fmt::Debug for HoundarrClient {
    /// Redacts `cookie_header` and `csrf_token` — both carry an authenticated
    /// Houndarr admin session and must never appear in logs (e.g. via
    /// `tracing::debug!(?client, ...)`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HoundarrClient")
            .field("base_url", &self.base_url)
            .field("client", &self.client)
            .field("cookie_header", &"<redacted>")
            .field("csrf_token", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize)]
struct StatusEnvelope {
    instances: Vec<StatusInstance>,
}

#[derive(Deserialize)]
struct StatusInstance {
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

/// Extract a cookie's plain value from a list of raw `Set-Cookie` header
/// values, e.g. `"houndarr_csrf=abc123; Path=/; SameSite=Lax"` -> `"abc123"`.
fn extract_cookie_value(set_cookie_headers: &[&str], name: &str) -> Option<String> {
    set_cookie_headers.iter().find_map(|h| {
        let (n, rest) = h.split_once('=')?;
        if n.trim() == name {
            Some(rest.split(';').next().unwrap_or("").to_string())
        } else {
            None
        }
    })
}

impl HoundarrClient {
    /// Log in and establish a session with Houndarr.
    ///
    /// `base_url` should be the root URL (e.g. `http://houndarr:8877`).
    ///
    /// # Errors
    ///
    /// Returns `ApiError::OperationFailed` on invalid credentials (401),
    /// rate limiting (429), or if the login response is missing either
    /// expected cookie. Returns `ApiError::ApiResponse` for any other
    /// non-redirect status. See [`ApiError`] for other failure modes.
    pub async fn new(base_url: &str, username: &str, password: &str) -> Result<Self, ApiError> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(ApiError::Request)?;

        let resp = client
            .post(format!("{base_url}/login"))
            .form(&[("username", username), ("password", password)])
            .send()
            .await
            .map_err(ApiError::Request)?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::OperationFailed {
                message: "Houndarr login rejected: invalid credentials".to_string(),
            });
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ApiError::OperationFailed {
                message: "Houndarr login rate-limited; retry later".to_string(),
            });
        }
        if !status.is_redirection() {
            let status = status.as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Houndarr error response body");
                String::new()
            });
            return Err(ApiError::ApiResponse { status, body });
        }

        let set_cookie: Vec<&str> = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| {
                v.to_str().ok().or_else(|| {
                    tracing::debug!(
                        "Houndarr Set-Cookie header was not valid UTF-8/ASCII, skipping"
                    );
                    None
                })
            })
            .collect();
        let session = extract_cookie_value(&set_cookie, SESSION_COOKIE_NAME).ok_or_else(|| {
            ApiError::OperationFailed {
                message: "Houndarr login succeeded but set no session cookie".to_string(),
            }
        })?;
        let csrf = extract_cookie_value(&set_cookie, CSRF_COOKIE_NAME).ok_or_else(|| {
            ApiError::OperationFailed {
                message: "Houndarr login succeeded but set no CSRF cookie".to_string(),
            }
        })?;

        Ok(Self {
            base_url,
            client,
            cookie_header: format!("{SESSION_COOKIE_NAME}={session}; {CSRF_COOKIE_NAME}={csrf}"),
            csrf_token: csrf,
        })
    }

    fn validate_kind(kind: &str) -> Result<(), ApiError> {
        if SUPPORTED_KINDS.contains(&kind) {
            Ok(())
        } else {
            Err(ApiError::OperationFailed {
                message: format!("Houndarr does not support *arr kind: {kind}"),
            })
        }
    }
}

impl CrossAppSync for HoundarrClient {
    /// # Errors
    ///
    /// Returns `ApiError::OperationFailed` if `kind` is not one Houndarr
    /// supports. See [`ApiError`] for other failure modes.
    async fn list_registered(&self, kind: &str) -> Result<Vec<String>, ApiError> {
        Self::validate_kind(kind)?;
        let resp = self
            .client
            .get(format!("{}/api/status", self.base_url))
            .header(reqwest::header::COOKIE, &self.cookie_header)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Houndarr error response body");
                String::new()
            });
            return Err(ApiError::ApiResponse { status, body });
        }

        let envelope: StatusEnvelope = resp.json().await.map_err(ApiError::Request)?;
        Ok(envelope
            .instances
            .into_iter()
            .filter(|i| i.kind == kind)
            .map(|i| i.name)
            .collect())
    }

    /// # Errors
    ///
    /// Returns `ApiError::OperationFailed` if `kind` is not one Houndarr
    /// supports. See [`ApiError`] for other failure modes.
    async fn register(&self, kind: &str, instance: &RegisteredArrInstance) -> Result<(), ApiError> {
        Self::validate_kind(kind)?;
        // The CSRF token MUST go in the X-CSRF-Token header, not a `csrf_token` form
        // field. Houndarr's CSRF check (auth/csrf.py) only calls `request.form()`
        // itself when the header is absent; that call consumes the body stream
        // before the route's own Form() params get parsed, so every other field
        // (name/type/url/api_key) comes back "missing" if csrf_token rides in the
        // form body instead.
        let resp = self
            .client
            .post(format!("{}/settings/instances", self.base_url))
            .header(reqwest::header::COOKIE, &self.cookie_header)
            .header("X-CSRF-Token", self.csrf_token.as_str())
            // `connection_verified` gates instance_submit.submit_create -- the
            // server independently re-probes the *arr instance regardless of this
            // flag's value (services/instance_submit.py's _verify_remote), so it's
            // not a trust-me bypass, just the UI's "I ran the test" checkbox state.
            .form(&[
                ("name", instance.name.as_str()),
                ("type", kind),
                ("url", instance.base_url.as_str()),
                ("api_key", instance.api_key.as_str()),
                ("connection_verified", "true"),
            ])
            .send()
            .await
            .map_err(ApiError::Request)?;

        let status = resp.status();
        if status.is_redirection() {
            // A redirect back to /login means the session expired or the CSRF token
            // rotated mid-sync -- that must not be conflated with the documented
            // "303 = created" success case, since the client built with
            // redirect::Policy::none() never follows it to find out otherwise.
            let redirects_to_login = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|location| location.contains("/login"));
            if redirects_to_login {
                return Err(ApiError::OperationFailed {
                    message: "Houndarr redirected to /login; session expired or CSRF token rotated mid-sync".to_string(),
                });
            }
            return Ok(());
        }
        if status.is_success() {
            Ok(())
        } else {
            let status = status.as_u16();
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "failed to read Houndarr error response body");
                String::new()
            });
            Err(ApiError::ApiResponse { status, body })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use wiremock::matchers::{body_string_contains, header, method, path};

    proptest! {
        // extract_cookie_value must recover the exact value for the named cookie
        // regardless of trailing attributes (Path, SameSite, HttpOnly, ...) or
        // other unrelated Set-Cookie headers present in the same response.
        #[test]
        fn extract_cookie_value_recovers_arbitrary_values(
            name in "[a-z_]{1,20}",
            value in "[a-zA-Z0-9._~-]{0,40}",
            attrs in "[a-zA-Z=/; ]{0,30}",
            other_name in "[a-z_]{1,20}",
            other_value in "[a-zA-Z0-9._~-]{0,20}",
        ) {
            prop_assume!(name != other_name);
            let target = format!("{name}={value}; {attrs}");
            let other = format!("{other_name}={other_value}");
            let headers = [other.as_str(), target.as_str()];

            let extracted = extract_cookie_value(&headers, &name);
            prop_assert_eq!(extracted, Some(value));
        }

        // A cookie name that never appears must yield None, not a panic or a
        // false match against an unrelated cookie's value.
        #[test]
        fn extract_cookie_value_returns_none_when_absent(
            headers in prop::collection::vec("[a-z_]{1,20}=[a-zA-Z0-9._~-]{0,20}", 0..5),
            missing_name in "[a-z_]{1,20}",
        ) {
            prop_assume!(headers.iter().all(|h| !h.starts_with(&format!("{missing_name}="))));
            let refs: Vec<&str> = headers.iter().map(String::as_str).collect();
            prop_assert_eq!(extract_cookie_value(&refs, &missing_name), None);
        }
    }
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mount_successful_login(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(
                ResponseTemplate::new(303)
                    .insert_header("Location", "/")
                    .append_header(
                        "Set-Cookie",
                        "houndarr_session=sess-token; Path=/; HttpOnly; SameSite=Lax",
                    )
                    .append_header(
                        "Set-Cookie",
                        "houndarr_csrf=csrf-token; Path=/; SameSite=Lax",
                    ),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn new_debug_output_does_not_leak_session_or_csrf() {
        let server = MockServer::start().await;
        mount_successful_login(&server).await;

        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .expect("should log in");
        let debug_output = format!("{client:?}");

        assert!(
            !debug_output.contains("sess-token"),
            "session cookie leaked into Debug output: {debug_output}"
        );
        assert!(
            !debug_output.contains("csrf-token"),
            "CSRF token leaked into Debug output: {debug_output}"
        );
    }

    #[tokio::test]
    async fn new_extracts_session_and_csrf_cookies_on_success() {
        let server = MockServer::start().await;
        mount_successful_login(&server).await;

        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .expect("should log in");

        assert_eq!(
            client.cookie_header,
            "houndarr_session=sess-token; houndarr_csrf=csrf-token"
        );
        assert_eq!(client.csrf_token, "csrf-token");
    }

    #[tokio::test]
    async fn new_rejects_invalid_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Invalid credentials."))
            .mount(&server)
            .await;

        let err = HoundarrClient::new(&server.uri(), "admin", "wrong")
            .await
            .unwrap_err();
        match err {
            ApiError::OperationFailed { message } => {
                assert!(message.contains("invalid credentials"), "got: {message}");
            }
            other => panic!("expected OperationFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn new_rejects_rate_limited_login() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(429).set_body_string("Too many attempts."))
            .mount(&server)
            .await;

        let err = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap_err();
        match err {
            ApiError::OperationFailed { message } => {
                assert!(message.contains("rate-limited"), "got: {message}");
            }
            other => panic!("expected OperationFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn new_errors_when_session_cookie_missing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(
                ResponseTemplate::new(303)
                    .insert_header("Location", "/")
                    .append_header("Set-Cookie", "houndarr_csrf=csrf-token; Path=/"),
            )
            .mount(&server)
            .await;

        let err = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap_err();
        match err {
            ApiError::OperationFailed { message } => {
                assert!(message.contains("session cookie"), "got: {message}");
            }
            other => panic!("expected OperationFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn list_registered_filters_by_kind_from_status_json() {
        let server = MockServer::start().await;
        mount_successful_login(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/status"))
            .and(header(
                "Cookie",
                "houndarr_session=sess-token; houndarr_csrf=csrf-token",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "instances": [
                    {"name": "Sonarr1", "type": "sonarr"},
                    {"name": "Radarr1", "type": "radarr"},
                    {"name": "Sonarr2", "type": "sonarr"},
                ],
                "recent_searches": []
            })))
            .mount(&server)
            .await;

        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap();
        let names = client.list_registered("sonarr").await.expect("should list");

        assert_eq!(names, vec!["Sonarr1".to_string(), "Sonarr2".to_string()]);
    }

    #[tokio::test]
    async fn list_registered_rejects_unsupported_kind() {
        let server = MockServer::start().await;
        mount_successful_login(&server).await;
        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap();
        let err = client.list_registered("plex").await.unwrap_err();
        match err {
            ApiError::OperationFailed { message } => assert!(message.contains("plex")),
            other => panic!("expected OperationFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn register_sends_csrf_token_as_header_not_form_field() {
        let server = MockServer::start().await;
        mount_successful_login(&server).await;
        Mock::given(method("POST"))
            .and(path("/settings/instances"))
            .and(header(
                "Cookie",
                "houndarr_session=sess-token; houndarr_csrf=csrf-token",
            ))
            .and(header("X-CSRF-Token", "csrf-token"))
            .and(body_string_contains("name=Sonarr1"))
            .and(body_string_contains("connection_verified=true"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<table>...</table>"))
            .mount(&server)
            .await;

        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap();
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
    async fn register_treats_303_as_success() {
        let server = MockServer::start().await;
        mount_successful_login(&server).await;
        Mock::given(method("POST"))
            .and(path("/settings/instances"))
            .respond_with(ResponseTemplate::new(303).insert_header("Location", "/settings"))
            .mount(&server)
            .await;

        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap();
        let instance = RegisteredArrInstance {
            name: "Radarr1".to_string(),
            base_url: "http://radarr:7878".to_string(),
            api_key: "radarr-key".to_string(),
        };
        client
            .register("radarr", &instance)
            .await
            .expect("should register");
    }

    #[tokio::test]
    async fn register_redirect_to_login_is_not_treated_as_success() {
        // A 303 to /login means the session expired mid-sync (or the CSRF token
        // rotated) -- it must not be conflated with the documented "303 = created"
        // success case.
        let server = MockServer::start().await;
        mount_successful_login(&server).await;
        Mock::given(method("POST"))
            .and(path("/settings/instances"))
            .respond_with(ResponseTemplate::new(303).insert_header("Location", "/login"))
            .mount(&server)
            .await;

        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap();
        let instance = RegisteredArrInstance {
            name: "Radarr1".to_string(),
            base_url: "http://radarr:7878".to_string(),
            api_key: "radarr-key".to_string(),
        };
        let err = client.register("radarr", &instance).await.unwrap_err();
        match err {
            ApiError::OperationFailed { message } => {
                assert!(message.contains("session"), "got: {message}");
            }
            other => panic!("expected OperationFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn register_returns_error_on_failure() {
        let server = MockServer::start().await;
        mount_successful_login(&server).await;
        Mock::given(method("POST"))
            .and(path("/settings/instances"))
            .respond_with(ResponseTemplate::new(422).set_body_string("Invalid URL"))
            .mount(&server)
            .await;

        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap();
        let instance = RegisteredArrInstance {
            name: "Radarr1".to_string(),
            base_url: "invalid".to_string(),
            api_key: "key".to_string(),
        };
        let err = client.register("radarr", &instance).await.unwrap_err();
        match err {
            ApiError::ApiResponse { status, .. } => assert_eq!(status, 422),
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn register_rejects_unsupported_kind() {
        let server = MockServer::start().await;
        mount_successful_login(&server).await;
        let client = HoundarrClient::new(&server.uri(), "admin", "hunter2")
            .await
            .unwrap();
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
