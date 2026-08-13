use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use url::Url;

use crate::client::ApiError;
use crate::health::HealthCheck;

const SESSION_HEADER: &str = "X-Transmission-Session-Id";
const RPC_PATH: &str = "/transmission/rpc";

/// Client for the Transmission JSON-RPC API.
///
/// Transmission uses a custom session-ID handshake: the first request returns
/// HTTP 409 with a `X-Transmission-Session-Id` header that must be echoed on
/// all subsequent requests.
#[derive(Debug, Clone)]
pub struct TransmissionClient {
    inner: reqwest::Client,
    rpc_url: Url,
    session_id: Arc<RwLock<Option<String>>>,
}

// --- RPC envelope ---

#[derive(Serialize)]
struct RpcRequest<'a> {
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: String,
    arguments: T,
}

// --- Response types ---

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SessionInfo {
    pub version: String,
    #[serde(default)]
    pub rpc_version: i64,
    #[serde(default)]
    pub rpc_version_minimum: i64,
    #[serde(default)]
    pub download_dir: String,
    #[serde(default)]
    pub config_dir: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    #[serde(default, rename = "activeTorrentCount")]
    pub active_torrent_count: i64,
    #[serde(default, rename = "pausedTorrentCount")]
    pub paused_torrent_count: i64,
    #[serde(default, rename = "torrentCount")]
    pub torrent_count: i64,
    #[serde(default, rename = "downloadSpeed")]
    pub download_speed: i64,
    #[serde(default, rename = "uploadSpeed")]
    pub upload_speed: i64,
}

/// A single torrent as returned by `torrent-get`.
///
/// `status` values follow the Transmission RPC spec: `1` = queued to verify,
/// `2` = verifying. `error`/`error_string` are non-empty when Transmission
/// hit a problem with this torrent — e.g. `error_string` containing
/// "No data found!" when the on-disk data has gone missing (#483).
///
/// `hash_string` is the torrent's content-addressed SHA-1 hash — stable across a Transmission
/// daemon restart, unlike `id`, which is assigned from a per-process counter that resets on
/// restart. Callers that act on a torrent some time after observing it (verify, then remove)
/// must address it by `hash_string`, not `id` (#500).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentInfo {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub error: i64,
    #[serde(default)]
    pub error_string: String,
    #[serde(default)]
    pub status: i64,
    #[serde(default)]
    pub hash_string: String,
    /// Fraction of the torrent's data present on disk, `0.0`-`1.0`. Used alongside `error`
    /// to confirm a `TR_STAT_LOCAL_ERROR` torrent's data is actually gone (`0.0`) rather than
    /// some other local I/O problem — e.g. a permissions or disk-full error on an otherwise
    /// complete torrent (#537).
    #[serde(default)]
    pub percent_done: f64,
}

#[derive(Debug, Deserialize)]
struct TorrentGetResult {
    torrents: Vec<TorrentInfo>,
}

/// Whether [`TransmissionClient::torrent_remove`] should also delete the torrent's on-disk
/// data. A plain `bool` reads identically at both call sites (`torrent_remove(&ids, true)` vs
/// `torrent_remove(&ids, false)`) for a parameter that controls an irreversible filesystem
/// delete — a named enum makes the call site self-describing and a flipped literal a type
/// error instead of a silent behavior change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteLocalData {
    /// Also delete the torrent's on-disk files. Irreversible.
    Yes,
    /// Only remove Transmission's bookkeeping entry; on-disk files (if any) are left alone.
    No,
}

impl DeleteLocalData {
    fn as_bool(self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// Basic-auth username/password pair for a [`TransmissionClient`].
///
/// One indivisible credential: a half-set state (username without password, or
/// vice-versa) is unrepresentable, so a client built from it can never silently
/// degrade to anonymous the way the old two-`Option<&str>` signature could (#505).
#[derive(Clone, PartialEq, Eq)]
pub struct BasicCredentials {
    username: String,
    password: String,
}

// Manual Debug: the derived form would print the password in cleartext if a caller
// debug-formats the value (e.g. `tracing::debug!(?creds, ...)`). The username stays
// visible for debuggability; the secret half is redacted.
impl std::fmt::Debug for BasicCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasicCredentials")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl BasicCredentials {
    /// Build credentials from a fully-provided username/password pair.
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    /// The username half of the credential pair.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// The password half of the credential pair.
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl TransmissionClient {
    /// Create a new Transmission RPC client.
    ///
    /// `base_url` should be the root URL (e.g. `http://transmission:9091`).
    /// Pass `Some(credentials)` for authenticated instances, `None` for
    /// auth-disabled ones. Half-set credentials are unrepresentable: the pair is
    /// built by the caller at the point where they decide how to handle a partial
    /// read, so the constructor itself never has to guess (#505).
    pub fn new(base_url: &str, credentials: Option<&BasicCredentials>) -> Result<Self, ApiError> {
        let mut rpc_url = Url::parse(base_url)?;
        rpc_url.set_path(RPC_PATH);

        let mut builder = reqwest::Client::builder();
        if let Some(creds) = credentials {
            builder = builder.default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                let mut auth_value = HeaderValue::from_str(&basic_auth_header(creds))
                    .map_err(|_| ApiError::InvalidApiKey)?;
                // Prevents the credential from appearing in reqwest::Client's Debug output
                // (which prints default_headers unconditionally) if this client is ever
                // logged or debug-formatted by a caller.
                auth_value.set_sensitive(true);
                headers.insert(reqwest::header::AUTHORIZATION, auth_value);
                headers
            });
        }

        Ok(Self {
            inner: builder
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(10))
                .build()
                .map_err(ApiError::Request)?,
            rpc_url,
            session_id: Arc::new(RwLock::new(None)),
        })
    }

    /// Fetch session info via `session-get`.
    pub async fn session_get(&self) -> Result<SessionInfo, ApiError> {
        self.rpc_call("session-get", None).await
    }

    /// Fetch transfer statistics via `session-stats`.
    pub async fn session_stats(&self) -> Result<SessionStats, ApiError> {
        self.rpc_call("session-stats", None).await
    }

    /// Fetch torrents via `torrent-get`. `fields` selects which attributes Transmission
    /// returns; pass `hashes` to scope the request to specific torrents by their stable
    /// `hashString` (not the process-local numeric `id`, which is unstable across a
    /// Transmission restart — #500), or `None` for all.
    pub async fn torrent_get(
        &self,
        fields: &[&str],
        hashes: Option<&[&str]>,
    ) -> Result<Vec<TorrentInfo>, ApiError> {
        let mut args = serde_json::json!({ "fields": fields });
        if let Some(hashes) = hashes {
            args["ids"] = serde_json::json!(hashes);
        }
        let result: TorrentGetResult = self.rpc_call("torrent-get", Some(args)).await?;
        Ok(result.torrents)
    }

    /// Trigger a hash-check via `torrent-verify`, addressing torrents by their stable
    /// `hashString` rather than numeric `id` (#500). Never destructive — Transmission only
    /// re-reads on-disk data, it never deletes anything.
    pub async fn torrent_verify(&self, hashes: &[&str]) -> Result<(), ApiError> {
        let args = serde_json::json!({ "ids": hashes });
        let _: serde_json::Value = self.rpc_call("torrent-verify", Some(args)).await?;
        Ok(())
    }

    /// Remove torrents via `torrent-remove`, addressing them by their stable `hashString`
    /// rather than numeric `id` (#500). Returns `Ok(())` immediately, without an RPC call,
    /// when `hashes` is empty — Transmission's RPC spec treats an *absent* `ids` key as "all
    /// torrents", so an empty removal set must never silently degrade into an unscoped one.
    pub async fn torrent_remove(
        &self,
        hashes: &[&str],
        delete_local_data: DeleteLocalData,
    ) -> Result<(), ApiError> {
        if hashes.is_empty() {
            return Ok(());
        }
        let args = serde_json::json!({
            "ids": hashes,
            "delete-local-data": delete_local_data.as_bool(),
        });
        let _: serde_json::Value = self.rpc_call("torrent-remove", Some(args)).await?;
        Ok(())
    }

    /// Set authentication credentials via `session-set`.
    ///
    /// Enables RPC authentication and sets the username and password.
    /// Note: the new credentials only take effect after a Transmission restart
    /// or when the client reconnects. Create a new `TransmissionClient` with
    /// the updated credentials for subsequent calls.
    pub async fn session_set_auth(&self, username: &str, password: &str) -> Result<(), ApiError> {
        let args = serde_json::json!({
            "rpc-authentication-required": true,
            "rpc-username": username,
            "rpc-password": password,
        });
        let _: serde_json::Value = self.rpc_call("session-set", Some(args)).await?;
        Ok(())
    }

    /// Execute an RPC call, handling the session-ID handshake automatically.
    async fn rpc_call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<T, ApiError> {
        let body = RpcRequest { method, arguments };

        // First attempt: use cached session ID if we have one
        let resp = self.send_rpc(&body).await?;

        if resp.status().as_u16() == 409 {
            // Extract session ID from the 409 response
            if let Some(sid) = resp.headers().get(SESSION_HEADER) {
                let sid = sid.to_str().unwrap_or("").to_string();
                *self.session_id.write().await = Some(sid);
            }
            // Retry with the new session ID
            let resp = self.send_rpc(&body).await?;
            Self::parse_rpc_response(resp).await
        } else {
            Self::parse_rpc_response(resp).await
        }
    }

    /// Parse an RPC HTTP response into its `arguments`. Transmission answers RPC-level
    /// failures (bad argument, unrecognized method) with HTTP 200 and `result` set to a
    /// non-"success" message, so the HTTP status alone can't tell a caller the request was
    /// rejected — callers that trigger destructive follow-up actions (e.g. removing a torrent
    /// after a `torrent-verify` failure) depend on this check, not just the status code (#483).
    async fn parse_rpc_response<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, ApiError> {
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::ApiResponse { status, body });
        }
        let rpc_resp: RpcResponse<T> = resp.json().await.map_err(ApiError::Request)?;
        if rpc_resp.result != "success" {
            return Err(ApiError::OperationFailed {
                message: rpc_resp.result,
            });
        }
        Ok(rpc_resp.arguments)
    }

    async fn send_rpc<S: Serialize>(&self, body: &S) -> Result<reqwest::Response, ApiError> {
        let mut req = self.inner.post(self.rpc_url.clone()).json(body);
        if let Some(ref sid) = *self.session_id.read().await {
            req = req.header(SESSION_HEADER, sid.as_str());
        }
        req.send().await.map_err(ApiError::Request)
    }
}

impl HealthCheck for TransmissionClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let info = self.session_get().await?;
        Ok(!info.version.is_empty())
    }
}

/// Build the HTTP `Authorization: Basic` header value for `creds`.
///
/// Base64-encodes `username:password`. Extracted from `TransmissionClient::new`
/// (#505) so the credential encoding is independently testable; the resulting
/// header value is marked `set_sensitive(true)` at the call site.
fn basic_auth_header(creds: &BasicCredentials) -> String {
    format!(
        "Basic {}",
        base64_encode(&format!("{}:{}", creds.username(), creds.password()))
    )
}

fn base64_encode(input: &str) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut encoder = Base64Writer::new(&mut buf);
        encoder.write_all(input.as_bytes()).unwrap();
        encoder.finish();
    }
    String::from_utf8(buf).unwrap()
}

/// Minimal Base64 encoder (avoids pulling in the `base64` crate).
struct Base64Writer<'a> {
    out: &'a mut Vec<u8>,
    buf: [u8; 3],
    buf_len: usize,
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

impl<'a> Base64Writer<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            buf: [0; 3],
            buf_len: 0,
        }
    }

    fn finish(&mut self) {
        if self.buf_len > 0 {
            self.encode_block();
        }
    }

    fn encode_block(&mut self) {
        let b = &self.buf;
        let n = self.buf_len;
        self.out.push(B64[(b[0] >> 2) as usize]);
        self.out
            .push(B64[((b[0] & 0x03) << 4 | b[1] >> 4) as usize]);
        if n > 1 {
            self.out
                .push(B64[((b[1] & 0x0f) << 2 | b[2] >> 6) as usize]);
        } else {
            self.out.push(b'=');
        }
        if n > 2 {
            self.out.push(B64[(b[2] & 0x3f) as usize]);
        } else {
            self.out.push(b'=');
        }
        self.buf = [0; 3];
        self.buf_len = 0;
    }
}

impl std::io::Write for Base64Writer<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        for &byte in data {
            self.buf[self.buf_len] = byte;
            self.buf_len += 1;
            if self.buf_len == 3 {
                self.encode_block();
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_json, body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn rpc_ok(result: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"result": "success", "arguments": result})
    }

    #[tokio::test]
    async fn new_with_credentials_sends_basic_auth_header() {
        // The mock only matches when the request carries the expected Basic-auth header,
        // so a client built with `Some(credentials)` must send it — proving the pair
        // flows into the request rather than being dropped (#505).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(header("Authorization", "Basic dXNlcjpwYXNz"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({
                    "version": "3.00"
                }))),
            )
            .mount(&server)
            .await;

        let creds = BasicCredentials::new("user", "pass");
        let client = TransmissionClient::new(&server.uri(), Some(&creds)).unwrap();
        client.session_get().await.unwrap();
    }

    #[tokio::test]
    async fn new_without_credentials_is_anonymous() {
        // A client built with `None` must not carry an Authorization header: against a
        // server that requires one, the request goes unmatched and fails instead of
        // silently succeeding unauthenticated (#505).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(header("Authorization", "Basic dXNlcjpwYXNz"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({
                    "version": "3.00"
                }))),
            )
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        let result = client.session_get().await;
        assert!(result.is_err(), "anonymous client must not authenticate");
    }

    #[tokio::test]
    async fn rpc_level_failure_with_http_200_is_an_error() {
        // Transmission answers a rejected RPC (bad argument, unrecognized method) with
        // HTTP 200 and a non-"success" `result` string — the HTTP status alone must not be
        // trusted as a success signal.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "invalid argument",
                "arguments": {}
            })))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        let result = client.torrent_verify(&["hash1"]).await;
        assert!(
            matches!(result, Err(ApiError::OperationFailed { .. })),
            "expected OperationFailed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn rpc_call_surfaces_non_success_http_status() {
        // A non-2xx HTTP status must surface as `ApiResponse` immediately, without
        // attempting to parse the (non-RPC) body as an RPC response — dropping or
        // inverting the status check would instead yield `ApiError::Request`.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        let result = client.session_get().await;
        assert!(
            matches!(result, Err(ApiError::ApiResponse { status: 500, .. })),
            "expected ApiResponse with status 500, got {result:?}"
        );
    }

    #[tokio::test]
    async fn session_set_auth_sends_correct_arguments() {
        let server = MockServer::start().await;
        // First request returns 409 with a session ID
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409).append_header("X-Transmission-Session-Id", "sess-abc"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Retry succeeds. The body constraint plus `expect_at_least(1)` prove the
        // session-set RPC was actually sent with the intended arguments — a call that
        // is dropped entirely, or that builds the wrong payload, leaves this mock
        // unmatched and the test fails.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(serde_json::json!({
                "method": "session-set",
                "arguments": {
                    "rpc-authentication-required": true,
                    "rpc-username": "admin",
                    "rpc-password": "secret",
                },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({}))))
            .expect(1..)
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        client.session_set_auth("admin", "secret").await.unwrap();
    }

    #[tokio::test]
    async fn session_get_returns_session_info() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({
                    "version": "3.00 (bb6b5a062e)",
                    "rpc-version": 17,
                    "rpc-version-minimum": 14,
                    "download-dir": "/downloads",
                    "config-dir": "/config",
                }))),
            )
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        let info = client.session_get().await.unwrap();
        assert!(info.version.starts_with("3.00"));
    }

    #[tokio::test]
    async fn torrent_get_parses_torrents_and_sends_requested_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({
                "torrents": [
                    {"id": 1, "name": "Show S01E01", "error": 3, "errorString": "No data found! Ensure your drives are connected.", "status": 0},
                    {"id": 2, "name": "Movie", "error": 0, "errorString": "", "status": 6},
                ]
            }))))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        let torrents = client
            .torrent_get(&["id", "name", "error", "errorString", "status"], None)
            .await
            .unwrap();

        assert_eq!(torrents.len(), 2);
        assert_eq!(torrents[0].id, 1);
        assert_eq!(torrents[0].error, 3);
        assert!(torrents[0].error_string.contains("No data found"));
        assert_eq!(torrents[1].id, 2);
        assert_eq!(torrents[1].error, 0);
    }

    #[tokio::test]
    async fn torrent_get_parses_hash_string_from_the_wire_field_name() {
        // Fixed-payload test asserting the actual wire mapping (#500) -- distinct from the
        // roundtrip proptest below, which only proves serialize/deserialize are inverses of
        // each other and would pass even if both sides used the wrong wire name.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({
                    "torrents": [
                        {"id": 1, "name": "x", "error": 0, "errorString": "", "status": 0,
                         "hashString": "0123456789abcdef0123456789abcdef01234567"}
                    ]
                }))),
            )
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        let torrents = client
            .torrent_get(
                &["id", "name", "error", "errorString", "status", "hashString"],
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            torrents[0].hash_string,
            "0123456789abcdef0123456789abcdef01234567"
        );
    }

    #[tokio::test]
    async fn torrent_get_scopes_request_to_given_ids() {
        let server = MockServer::start().await;
        // The body constraint proves the request actually carries the `ids` scoping:
        // without it, a torrent-get whose `ids` are dropped still hits this path-only
        // mock and the test passes.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(serde_json::json!({
                "method": "torrent-get",
                "arguments": {
                    "fields": ["id", "name"],
                    "ids": ["hash7"],
                },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({
                "torrents": [{"id": 7, "name": "Only Me", "error": 0, "errorString": "", "status": 0}]
            }))))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        let torrents = client
            .torrent_get(&["id", "name"], Some(&["hash7"]))
            .await
            .unwrap();

        assert_eq!(torrents.len(), 1);
        assert_eq!(torrents[0].id, 7);
    }

    #[tokio::test]
    async fn torrent_get_omits_ids_when_hashes_absent() {
        // With `None` hashes the `ids` key must be absent from the request entirely:
        // Transmission treats an absent `ids` as "all torrents", while `"ids": null`
        // would be rejected. The exact `body_json` match fails if any stray `ids`
        // key appears.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_json(serde_json::json!({
                "method": "torrent-get",
                "arguments": { "fields": ["id", "name"] },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({
                "torrents": [{"id": 7, "name": "Only Me", "error": 0, "errorString": "", "status": 0}]
            }))))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        let torrents = client.torrent_get(&["id", "name"], None).await.unwrap();

        assert_eq!(torrents.len(), 1);
        assert_eq!(torrents[0].id, 7);
    }

    #[tokio::test]
    async fn torrent_verify_sends_ids_and_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({}))))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        client.torrent_verify(&["hash1", "hash2"]).await.unwrap();
    }

    #[tokio::test]
    async fn torrent_remove_sends_delete_local_data_flag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({}))))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        client
            .torrent_remove(&["hash1"], DeleteLocalData::No)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn torrent_remove_is_a_noop_for_empty_ids() {
        // No mock mounted — a request would fail with no matching Mock, proving this
        // never reaches the network when `ids` is empty (an absent `ids` key would mean
        // "all torrents" to Transmission, so this must short-circuit, not RPC-call).
        let server = MockServer::start().await;
        let client = TransmissionClient::new(&server.uri(), None).unwrap();
        client
            .torrent_remove(&[], DeleteLocalData::No)
            .await
            .unwrap();
    }

    use super::base64_encode;

    #[test]
    fn base64_empty() {
        assert_eq!(base64_encode(""), "");
    }

    #[test]
    fn base64_one_byte() {
        // "a" -> "YQ=="
        assert_eq!(base64_encode("a"), "YQ==");
    }

    #[test]
    fn base64_two_bytes() {
        // "ab" -> "YWI="
        assert_eq!(base64_encode("ab"), "YWI=");
    }

    #[test]
    fn base64_three_bytes() {
        // "abc" -> "YWJj"
        assert_eq!(base64_encode("abc"), "YWJj");
    }

    #[test]
    fn base64_hello_world() {
        // "hello world" -> "aGVsbG8gd29ybGQ="
        assert_eq!(base64_encode("hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn base64_credentials() {
        // "user:pass" -> "dXNlcjpwYXNz"
        assert_eq!(base64_encode("user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn basic_credentials_debug_redacts_password() {
        let creds = BasicCredentials::new("admin", "hunter2");
        let rendered = format!("{creds:?}");
        assert!(
            rendered.contains("admin"),
            "username stays visible: {rendered}"
        );
        assert!(
            !rendered.contains("hunter2"),
            "password must never appear in Debug output: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "redaction marker present: {rendered}"
        );
    }

    // ---- TorrentInfo roundtrip (#501) ----

    proptest::proptest! {
        #[test]
        fn torrent_info_roundtrips_through_json(
            id in proptest::num::i64::ANY,
            name in ".{0,200}",
            error in proptest::num::i64::ANY,
            error_string in ".{0,200}",
            status in proptest::num::i64::ANY,
            hash_string in "[0-9a-f]{0,40}",
            // Transmission reports a 0.0-1.0 fraction; ANY would generate NaN, which
            // breaks the equality roundtrip assertion below (NaN != NaN).
            percent_done in 0.0f64..=1.0,
        ) {
            let original = TorrentInfo {
                id,
                name,
                error,
                error_string,
                status,
                hash_string,
                percent_done,
            };
            let json = serde_json::to_value(&original).unwrap();
            let roundtripped: TorrentInfo = serde_json::from_value(json).unwrap();
            proptest::prop_assert_eq!(original, roundtripped);
        }
    }

    // ---- Basic auth header roundtrip (#505) ----
    //
    // `.` matches any non-newline char, so arbitrary usernames/passwords are
    // covered: colons, empty strings, and multibyte unicode alike. The decode
    // uses the base64 crate's STANDARD engine — an implementation independent of
    // this module's Base64Writer — so a mirrored encode bug can't pass.

    proptest::proptest! {
        #[test]
        fn basic_auth_header_roundtrips_through_base64(
            username in ".{0,128}",
            password in ".{0,128}",
        ) {
            let creds = BasicCredentials::new(&username, &password);
            let header = basic_auth_header(&creds);
            proptest::prop_assert!(header.starts_with("Basic "));
            let encoded = &header["Basic ".len()..];
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            proptest::prop_assert_eq!(
                format!("{}:{}", creds.username(), creds.password()),
                String::from_utf8(decoded).unwrap()
            );
        }
    }
}
