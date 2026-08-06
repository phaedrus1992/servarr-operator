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
#[derive(Debug, Clone, Deserialize)]
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

impl TransmissionClient {
    /// Create a new Transmission RPC client.
    ///
    /// `base_url` should be the root URL (e.g. `http://transmission:9091`).
    /// For authenticated instances, pass `username` and `password`.
    pub fn new(
        base_url: &str,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, ApiError> {
        let mut rpc_url = Url::parse(base_url)?;
        rpc_url.set_path(RPC_PATH);

        let mut builder = reqwest::Client::builder();
        if let (Some(user), Some(pass)) = (username, password) {
            builder = builder.default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                let credentials = base64_encode(&format!("{user}:{pass}"));
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {credentials}"))
                        .map_err(|_| ApiError::InvalidApiKey)?,
                );
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
    /// returns; pass `ids` to scope the request to specific torrents, or `None` for all.
    pub async fn torrent_get(
        &self,
        fields: &[&str],
        ids: Option<&[i64]>,
    ) -> Result<Vec<TorrentInfo>, ApiError> {
        let mut args = serde_json::json!({ "fields": fields });
        if let Some(ids) = ids {
            args["ids"] = serde_json::json!(ids);
        }
        let result: TorrentGetResult = self.rpc_call("torrent-get", Some(args)).await?;
        Ok(result.torrents)
    }

    /// Trigger a hash-check via `torrent-verify`. Never destructive — Transmission only
    /// re-reads on-disk data, it never deletes anything.
    pub async fn torrent_verify(&self, ids: &[i64]) -> Result<(), ApiError> {
        let args = serde_json::json!({ "ids": ids });
        let _: serde_json::Value = self.rpc_call("torrent-verify", Some(args)).await?;
        Ok(())
    }

    /// Remove torrents via `torrent-remove`. Returns `Ok(())` immediately, without an RPC
    /// call, when `ids` is empty — Transmission's RPC spec treats an *absent* `ids` key as
    /// "all torrents", so an empty removal set must never silently degrade into an
    /// unscoped one.
    pub async fn torrent_remove(
        &self,
        ids: &[i64],
        delete_local_data: DeleteLocalData,
    ) -> Result<(), ApiError> {
        if ids.is_empty() {
            return Ok(());
        }
        let args = serde_json::json!({
            "ids": ids,
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn rpc_ok(result: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"result": "success", "arguments": result})
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

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        let result = client.torrent_verify(&[1]).await;
        assert!(
            matches!(result, Err(ApiError::OperationFailed { .. })),
            "expected OperationFailed, got {result:?}"
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
        // Retry succeeds
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({}))))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
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

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
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

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
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
    async fn torrent_get_scopes_request_to_given_ids() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({
                "torrents": [{"id": 7, "name": "Only Me", "error": 0, "errorString": "", "status": 0}]
            }))))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        let torrents = client
            .torrent_get(&["id", "name"], Some(&[7]))
            .await
            .unwrap();

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

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        client.torrent_verify(&[1, 2]).await.unwrap();
    }

    #[tokio::test]
    async fn torrent_remove_sends_delete_local_data_flag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_ok(serde_json::json!({}))))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        client
            .torrent_remove(&[1], DeleteLocalData::No)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn torrent_remove_is_a_noop_for_empty_ids() {
        // No mock mounted — a request would fail with no matching Mock, proving this
        // never reaches the network when `ids` is empty (an absent `ids` key would mean
        // "all torrents" to Transmission, so this must short-circuit, not RPC-call).
        let server = MockServer::start().await;
        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
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
}
