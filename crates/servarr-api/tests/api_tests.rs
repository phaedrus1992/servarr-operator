use servarr_api::HealthCheck;
use servarr_api::{
    ApiError, AppKind, HttpClient, JellyfinClient, OverseerrClient, PlexClient, ProwlarrClient,
    SabnzbdClient, SecretError, ServarrClient, TransmissionClient,
};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// HttpClient tests
// ---------------------------------------------------------------------------

mod http_client {
    use super::*;

    #[test]
    fn new_with_valid_url() {
        let client = HttpClient::new("http://localhost:8080", Some("test-key"));
        assert!(client.is_ok());
    }

    #[test]
    fn new_with_invalid_url() {
        let result = HttpClient::new("not a url", None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ApiError::InvalidUrl(_)));
    }

    #[test]
    fn new_with_non_ascii_api_key() {
        let result = HttpClient::new("http://localhost:8080", Some("key\x01bad"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidApiKey),
            "expected InvalidApiKey, got: {err}"
        );
    }

    #[test]
    fn base_url_returns_parsed_url() {
        let client = HttpClient::new("http://example.com:9090/", None).unwrap();
        assert_eq!(client.base_url().as_str(), "http://example.com:9090/");
    }

    #[test]
    fn debug_impl_shows_base_url() {
        let client = HttpClient::new("http://example.com:9090/", None).unwrap();
        let debug = format!("{client:?}");
        assert!(
            debug.contains("http://example.com:9090/"),
            "Debug output should contain base_url, got: {debug}"
        );
        assert!(
            debug.contains("HttpClient"),
            "Debug output should contain struct name, got: {debug}"
        );
    }

    #[tokio::test]
    async fn get_returns_json_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/system/status"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "4.0.0"})),
            )
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), Some("test-key")).unwrap();
        let resp: serde_json::Value = client.get("api/v3/system/status").await.unwrap();
        assert_eq!(resp["version"], "4.0.0");
    }

    #[tokio::test]
    async fn get_returns_api_error_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/system/status"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let result: Result<serde_json::Value, _> = client.get("api/v3/system/status").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ApiError::ApiResponse { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "internal error");
            }
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn post_sends_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/command"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 1, "name": "RefreshSeries"})),
            )
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let body = serde_json::json!({"name": "RefreshSeries"});
        let resp: serde_json::Value = client.post("api/v3/command", &body).await.unwrap();
        assert_eq!(resp["name"], "RefreshSeries");
    }

    #[tokio::test]
    async fn put_sends_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v3/series/1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 1, "title": "Updated"})),
            )
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let body = serde_json::json!({"id": 1, "title": "Updated"});
        let resp: serde_json::Value = client.put("api/v3/series/1", &body).await.unwrap();
        assert_eq!(resp["title"], "Updated");
    }

    #[tokio::test]
    async fn delete_succeeds_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v3/series/1"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let result = client.delete("api/v3/series/1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn delete_returns_error_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v3/series/999"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let result = client.delete("api/v3/series/999").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ApiError::ApiResponse { status, body } => {
                assert_eq!(status, 404);
                assert_eq!(body, "not found");
            }
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }
}

// ---------------------------------------------------------------------------
// TransmissionClient tests
// ---------------------------------------------------------------------------

mod transmission_client {
    use super::*;

    #[test]
    fn new_constructs_without_credentials() {
        let client = TransmissionClient::new("http://localhost:9091", None, None);
        assert!(client.is_ok());
    }

    #[test]
    fn new_constructs_with_credentials() {
        let client =
            TransmissionClient::new("http://localhost:9091", Some("admin"), Some("secret"));
        assert!(client.is_ok());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let result = TransmissionClient::new("not a url", None, None);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn session_get_with_409_handshake() {
        let server = MockServer::start().await;
        let session_id = "test-session-id-12345";

        // First request returns 409 with session ID header
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409).append_header("X-Transmission-Session-Id", session_id),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        // Second request (with session ID) returns 200 with session info
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "version": "4.0.5",
                    "rpc-version": 18,
                    "rpc-version-minimum": 14,
                    "download-dir": "/downloads",
                    "config-dir": "/config"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        let info = client.session_get().await.unwrap();
        assert_eq!(info.version, "4.0.5");
        assert_eq!(info.rpc_version, 18);
        assert_eq!(info.download_dir, "/downloads");
    }

    #[tokio::test]
    async fn session_stats_returns_stats() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "activeTorrentCount": 3,
                    "pausedTorrentCount": 1,
                    "torrentCount": 4,
                    "downloadSpeed": 1048576,
                    "uploadSpeed": 524288
                }
            })))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        let stats = client.session_stats().await.unwrap();
        assert_eq!(stats.active_torrent_count, 3);
        assert_eq!(stats.paused_torrent_count, 1);
        assert_eq!(stats.torrent_count, 4);
        assert_eq!(stats.download_speed, 1_048_576);
        assert_eq!(stats.upload_speed, 524_288);
    }

    #[tokio::test]
    async fn health_check_healthy_when_version_present() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "version": "4.0.5"
                }
            })))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn health_check_unhealthy_when_version_empty() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "version": ""
                }
            })))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }
}

// ---------------------------------------------------------------------------
// SabnzbdClient tests
// ---------------------------------------------------------------------------

mod sabnzbd_client {
    use super::*;

    #[test]
    fn new_constructs_client() {
        let client = SabnzbdClient::new("http://localhost:8080", "my-api-key");
        assert!(client.is_ok());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let result = SabnzbdClient::new("not a url", "key");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn version_returns_version_string() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "4.2.1"})),
            )
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        let version = client.version().await.unwrap();
        assert_eq!(version, "4.2.1");
    }

    #[tokio::test]
    async fn queue_status_returns_queue() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "queue": {
                    "status": "Downloading",
                    "speed": "10.5 M",
                    "sizeleft": "1.2 GB",
                    "mb": "5000.00",
                    "mbleft": "1200.00",
                    "noofslots_total": "5"
                }
            })))
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        let queue = client.queue_status().await.unwrap();
        assert_eq!(queue.status, "Downloading");
        assert_eq!(queue.speed, "10.5 M");
        assert_eq!(queue.size_left, "1.2 GB");
        assert_eq!(queue.total_mb, "5000.00");
        assert_eq!(queue.mb_left, "1200.00");
        assert_eq!(queue.total_slots, "5");
    }

    #[tokio::test]
    async fn server_stats_returns_stats() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 1024000,
                "servers": {
                    "news.example.com": {
                        "total": 1024000
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        let stats = client.server_stats().await.unwrap();
        assert_eq!(stats.total, 1_024_000);
        assert!(stats.servers.is_object());
    }

    #[tokio::test]
    async fn health_check_healthy() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "4.2.1"})),
            )
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn health_check_unhealthy_when_version_empty() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": ""})),
            )
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }
}

// ---------------------------------------------------------------------------
// PlexClient tests
// ---------------------------------------------------------------------------

mod plex_client {
    use super::*;

    #[test]
    fn new_constructs_client() {
        let client = PlexClient::new("http://localhost:32400");
        assert!(client.is_ok());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let result = PlexClient::new("not a url");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn is_healthy_returns_true_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/identity"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<MediaContainer size=\"0\"/>"),
            )
            .mount(&server)
            .await;

        let client = PlexClient::new(&server.uri()).unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn is_healthy_returns_false_on_500() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/identity"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = PlexClient::new(&server.uri()).unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }
}

// ---------------------------------------------------------------------------
// JellyfinClient tests
// ---------------------------------------------------------------------------

mod jellyfin_client {
    use super::*;

    #[test]
    fn new_constructs_client() {
        let client = JellyfinClient::new("http://localhost:8096");
        assert!(client.is_ok());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let result = JellyfinClient::new("not a url");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn is_healthy_returns_true_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Healthy"))
            .mount(&server)
            .await;

        let client = JellyfinClient::new(&server.uri()).unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn is_healthy_returns_false_on_500() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = JellyfinClient::new(&server.uri()).unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }
}

// ---------------------------------------------------------------------------
// ServarrClient (v3 SDK) integration tests via wiremock
// ---------------------------------------------------------------------------

mod servarr_client {
    use super::*;

    #[tokio::test]
    async fn system_status_returns_parsed_response() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v3/system/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "appName": "Sonarr",
                "version": "4.0.0",
                "buildTime": "2024-01-01T00:00:00Z",
                "isDebug": false,
                "isProduction": true,
                "isAdmin": false,
                "isUserInteractive": false,
                "startupPath": "/opt/sonarr",
                "appData": "/config",
                "osName": "Linux",
                "osVersion": "5.15",
                "runtimeName": ".NET",
                "runtimeVersion": "8.0.0"
            })))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        let status = client.system_status().await.unwrap();
        assert_eq!(status.app_name, "Sonarr");
        assert_eq!(status.version, "4.0.0");
        assert!(status.is_production);
        assert!(!status.is_debug);
        assert_eq!(status.os_name, "Linux");
    }

    #[tokio::test]
    async fn health_returns_parsed_response() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v3/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "source": "test",
                    "type": "ok",
                    "message": "healthy",
                    "wikiUrl": "https://wiki.example.com"
                }
            ])))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        let checks = client.health().await.unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].source, "test");
        assert_eq!(checks[0].check_type, "ok");
        assert_eq!(checks[0].message, "healthy");
    }

    #[tokio::test]
    async fn create_backup_returns_backup() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v3/system/backup"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1,
                "name": "sonarr_backup_2024.zip",
                "path": "/backups/sonarr_backup_2024.zip",
                "size": 1048576,
                "time": "2024-01-15T10:30:00Z"
            })))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        let backup = client.create_backup().await.unwrap();
        assert_eq!(backup.id, 1);
        assert_eq!(backup.name, "sonarr_backup_2024.zip");
        assert_eq!(backup.path, "/backups/sonarr_backup_2024.zip");
        assert_eq!(backup.size, 1_048_576);
    }

    #[tokio::test]
    async fn list_backups_returns_array() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v3/system/backup"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1,
                    "name": "backup_1.zip",
                    "path": "/backups/backup_1.zip",
                    "size": 500000,
                    "time": "2024-01-10T08:00:00Z"
                },
                {
                    "id": 2,
                    "name": "backup_2.zip",
                    "path": "/backups/backup_2.zip",
                    "size": 600000,
                    "time": "2024-01-15T08:00:00Z"
                }
            ])))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        let backups = client.list_backups().await.unwrap();
        assert_eq!(backups.len(), 2);
        assert_eq!(backups[0].id, 1);
        assert_eq!(backups[0].name, "backup_1.zip");
        assert_eq!(backups[1].id, 2);
        assert_eq!(backups[1].name, "backup_2.zip");
    }

    #[tokio::test]
    async fn is_healthy_returns_true_when_version_present() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v3/system/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "appName": "Sonarr",
                "version": "4.0.0"
            })))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn is_healthy_returns_false_when_version_empty() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v3/system/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "appName": "Sonarr",
                "version": ""
            })))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn updates_returns_array() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v3/update"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "version": "4.1.0",
                    "installed": false,
                    "installable": true,
                    "latest": true
                },
                {
                    "version": "4.0.0",
                    "installed": true,
                    "installable": false,
                    "latest": false
                }
            ])))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        let updates = client.updates().await.unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].version, "4.1.0");
        assert!(!updates[0].installed);
        assert!(updates[0].installable);
        assert!(updates[0].latest);
        assert_eq!(updates[1].version, "4.0.0");
        assert!(updates[1].installed);
    }

    #[tokio::test]
    async fn restore_backup_succeeds() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v3/system/backup/restore/42"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        let result = client.restore_backup(42).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn delete_backup_succeeds() {
        let server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path("/api/v3/system/backup/7"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = ServarrClient::new(&server.uri(), "test-key", AppKind::Sonarr).unwrap();
        let result = client.delete_backup(7).await;
        assert!(result.is_ok());
    }
}

// ---------------------------------------------------------------------------
// ServarrClient multi-kind integration tests via wiremock
// ---------------------------------------------------------------------------

mod servarr_client_multi_kind {
    use super::*;

    /// Mount all standard GET/POST/DELETE mocks for a servarr instance.
    ///
    /// Sonarr/Radarr SDKs use `/api/v3/` while Lidarr/Prowlarr SDKs use
    /// `/api/v1/`, so we match both with `path_regex`.
    async fn mount_standard_mocks(server: &MockServer, app_name: &str) {
        let status_json = serde_json::json!({
            "appName": app_name,
            "version": "5.0.0",
            "buildTime": "2025-01-01T00:00:00Z",
            "isDebug": false,
            "isProduction": true,
            "isAdmin": false,
            "isUserInteractive": false,
            "startupPath": "/opt/app",
            "appData": "/config",
            "osName": "Linux",
            "osVersion": "6.1",
            "runtimeName": ".NET",
            "runtimeVersion": "8.0.0"
        });

        let health_json = serde_json::json!([{
            "source": "test-source",
            "type": "ok",
            "message": "all good",
            "wikiUrl": "https://wiki.example.com"
        }]);

        let backup_json = serde_json::json!([{
            "id": 10,
            "name": "backup_2025.zip",
            "path": "/backups/backup_2025.zip",
            "size": 999999,
            "time": "2025-06-01T12:00:00Z"
        }]);

        let update_json = serde_json::json!([{
            "version": "5.1.0",
            "installed": false,
            "installable": true,
            "latest": true
        }]);

        let rootfolder_json = serde_json::json!([{
            "id": 1,
            "path": "/media",
            "accessible": true,
            "freeSpace": 50000000000_i64
        }]);

        // GET system/status
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v[13]/system/status$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(status_json))
            .mount(server)
            .await;

        // GET health
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v[13]/health$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(health_json))
            .mount(server)
            .await;

        // GET system/backup
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v[13]/system/backup$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(backup_json))
            .mount(server)
            .await;

        // GET update
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v[13]/update$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(update_json))
            .mount(server)
            .await;

        // GET rootfolder
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v[13]/rootfolder$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rootfolder_json))
            .mount(server)
            .await;

        // POST system/backup/restore/{id}
        Mock::given(method("POST"))
            .and(path_regex(r"^/api/v[13]/system/backup/restore/10$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;

        // DELETE system/backup/{id}
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/api/v[13]/system/backup/10$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }

    // -- Radarr ---------------------------------------------------------------

    #[tokio::test]
    async fn radarr_system_status() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Radarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Radarr).unwrap();
        let status = client.system_status().await.unwrap();
        assert_eq!(status.app_name, "Radarr");
        assert_eq!(status.version, "5.0.0");
        assert!(status.is_production);
        assert_eq!(status.os_name, "Linux");
    }

    #[tokio::test]
    async fn radarr_health() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Radarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Radarr).unwrap();
        let checks = client.health().await.unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].source, "test-source");
        assert_eq!(checks[0].check_type, "ok");
        assert_eq!(checks[0].message, "all good");
    }

    #[tokio::test]
    async fn radarr_list_backups() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Radarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Radarr).unwrap();
        let backups = client.list_backups().await.unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].id, 10);
        assert_eq!(backups[0].name, "backup_2025.zip");
    }

    #[tokio::test]
    async fn radarr_updates() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Radarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Radarr).unwrap();
        let updates = client.updates().await.unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].version, "5.1.0");
        assert!(updates[0].latest);
    }

    #[tokio::test]
    async fn radarr_root_folder() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Radarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Radarr).unwrap();
        let folders = client.root_folder().await.unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].path, "/media");
        assert!(folders[0].accessible);
    }

    #[tokio::test]
    async fn radarr_restore_backup() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Radarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Radarr).unwrap();
        assert!(client.restore_backup(10).await.is_ok());
    }

    #[tokio::test]
    async fn radarr_delete_backup() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Radarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Radarr).unwrap();
        assert!(client.delete_backup(10).await.is_ok());
    }

    // -- Lidarr ---------------------------------------------------------------

    #[tokio::test]
    async fn lidarr_system_status() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Lidarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Lidarr).unwrap();
        let status = client.system_status().await.unwrap();
        assert_eq!(status.app_name, "Lidarr");
        assert_eq!(status.version, "5.0.0");
        assert!(status.is_production);
    }

    #[tokio::test]
    async fn lidarr_health() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Lidarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Lidarr).unwrap();
        let checks = client.health().await.unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].source, "test-source");
        assert_eq!(checks[0].message, "all good");
    }

    #[tokio::test]
    async fn lidarr_list_backups() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Lidarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Lidarr).unwrap();
        let backups = client.list_backups().await.unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].id, 10);
    }

    #[tokio::test]
    async fn lidarr_updates() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Lidarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Lidarr).unwrap();
        let updates = client.updates().await.unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].version, "5.1.0");
    }

    #[tokio::test]
    async fn lidarr_root_folder() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Lidarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Lidarr).unwrap();
        let folders = client.root_folder().await.unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].path, "/media");
    }

    #[tokio::test]
    async fn lidarr_restore_backup() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Lidarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Lidarr).unwrap();
        assert!(client.restore_backup(10).await.is_ok());
    }

    #[tokio::test]
    async fn lidarr_delete_backup() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Lidarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Lidarr).unwrap();
        assert!(client.delete_backup(10).await.is_ok());
    }

    // -- Prowlarr -------------------------------------------------------------

    #[tokio::test]
    async fn prowlarr_system_status() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Prowlarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Prowlarr).unwrap();
        let status = client.system_status().await.unwrap();
        assert_eq!(status.app_name, "Prowlarr");
        assert_eq!(status.version, "5.0.0");
        assert!(status.is_production);
    }

    #[tokio::test]
    async fn prowlarr_health() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Prowlarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Prowlarr).unwrap();
        let checks = client.health().await.unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].source, "test-source");
        assert_eq!(checks[0].message, "all good");
    }

    #[tokio::test]
    async fn prowlarr_list_backups() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Prowlarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Prowlarr).unwrap();
        let backups = client.list_backups().await.unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].id, 10);
    }

    #[tokio::test]
    async fn prowlarr_updates() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Prowlarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Prowlarr).unwrap();
        let updates = client.updates().await.unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].version, "5.1.0");
    }

    #[tokio::test]
    async fn prowlarr_root_folder_returns_empty() {
        let server = MockServer::start().await;
        // No rootfolder mock needed -- Prowlarr returns Vec::new() without hitting the server.

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Prowlarr).unwrap();
        let folders = client.root_folder().await.unwrap();
        assert!(
            folders.is_empty(),
            "Prowlarr should return empty root folder list"
        );
    }

    #[tokio::test]
    async fn prowlarr_restore_backup() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Prowlarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Prowlarr).unwrap();
        assert!(client.restore_backup(10).await.is_ok());
    }

    #[tokio::test]
    async fn prowlarr_delete_backup() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Prowlarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Prowlarr).unwrap();
        assert!(client.delete_backup(10).await.is_ok());
    }

    // -- Sonarr root_folder (complete coverage for all 4 kinds) ---------------

    #[tokio::test]
    async fn sonarr_root_folder() {
        let server = MockServer::start().await;
        mount_standard_mocks(&server, "Sonarr").await;

        let client = ServarrClient::new(&server.uri(), "test-api-key", AppKind::Sonarr).unwrap();
        let folders = client.root_folder().await.unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].path, "/media");
        assert!(folders[0].accessible);
        assert_eq!(folders[0].free_space, 50_000_000_000);
    }
}

// ---------------------------------------------------------------------------
// OverseerrClient integration tests via wiremock
// ---------------------------------------------------------------------------

mod overseerr_client {
    use super::*;

    fn make_sonarr_settings_json() -> serde_json::Value {
        serde_json::json!({
            "id": 1,
            "name": "Sonarr Main",
            "hostname": "sonarr",
            "port": 8989,
            "apiKey": "abc123",
            "useSsl": false,
            "activeProfileId": 1,
            "activeProfileName": "Any",
            "activeDirectory": "/tv",
            "is4k": false,
            "enableSeasonFolders": true,
            "isDefault": true
        })
    }

    fn make_radarr_settings_json() -> serde_json::Value {
        serde_json::json!({
            "id": 1,
            "name": "Radarr Main",
            "hostname": "radarr",
            "port": 7878,
            "apiKey": "xyz789",
            "useSsl": false,
            "activeProfileId": 1,
            "activeProfileName": "Any",
            "activeDirectory": "/movies",
            "is4k": false,
            "minimumAvailability": "released",
            "isDefault": true
        })
    }

    #[tokio::test]
    async fn list_sonarr_returns_array() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/settings/sonarr"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([make_sonarr_settings_json()])),
            )
            .mount(&server)
            .await;

        let client = OverseerrClient::new(&server.uri(), "test-key");
        let result = client.list_sonarr().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "Sonarr Main");
        assert_eq!(result[0].hostname, "sonarr");
        assert_eq!(result[0].port, 8989.0);
    }

    #[tokio::test]
    async fn create_sonarr_returns_settings() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/settings/sonarr"))
            .respond_with(ResponseTemplate::new(201).set_body_json(make_sonarr_settings_json()))
            .mount(&server)
            .await;

        let settings = overseerr::models::SonarrSettings::new(
            "Sonarr Main".to_string(),
            "sonarr".to_string(),
            8989.0,
            "abc123".to_string(),
            false,
            1.0,
            "Any".to_string(),
            "/tv".to_string(),
            false,
            true,
            true,
        );

        let client = OverseerrClient::new(&server.uri(), "test-key");
        let result = client.create_sonarr(settings).await.unwrap();
        assert_eq!(result.name, "Sonarr Main");
        assert_eq!(result.id, Some(1.0));
    }

    #[tokio::test]
    async fn list_radarr_returns_array() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/settings/radarr"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([make_radarr_settings_json()])),
            )
            .mount(&server)
            .await;

        let client = OverseerrClient::new(&server.uri(), "test-key");
        let result = client.list_radarr().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "Radarr Main");
        assert_eq!(result[0].hostname, "radarr");
        assert_eq!(result[0].port, 7878.0);
    }

    #[tokio::test]
    async fn create_radarr_returns_settings() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/settings/radarr"))
            .respond_with(ResponseTemplate::new(201).set_body_json(make_radarr_settings_json()))
            .mount(&server)
            .await;

        let settings = overseerr::models::RadarrSettings::new(
            "Radarr Main".to_string(),
            "radarr".to_string(),
            7878.0,
            "xyz789".to_string(),
            false,
            1.0,
            "Any".to_string(),
            "/movies".to_string(),
            false,
            "released".to_string(),
            true,
        );

        let client = OverseerrClient::new(&server.uri(), "test-key");
        let result = client.create_radarr(settings).await.unwrap();
        assert_eq!(result.name, "Radarr Main");
        assert_eq!(result.id, Some(1.0));
    }

    #[tokio::test]
    async fn delete_sonarr_returns_ok() {
        let server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path("/settings/sonarr/5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(make_sonarr_settings_json()))
            .mount(&server)
            .await;

        let client = OverseerrClient::new(&server.uri(), "test-key");
        let result = client.delete_sonarr(5).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn delete_radarr_returns_ok() {
        let server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path("/settings/radarr/3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(make_radarr_settings_json()))
            .mount(&server)
            .await;

        let client = OverseerrClient::new(&server.uri(), "test-key");
        let result = client.delete_radarr(3).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_sonarr_returns_settings() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/settings/sonarr/2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(make_sonarr_settings_json()))
            .mount(&server)
            .await;

        let settings = overseerr::models::SonarrSettings::new(
            "Sonarr Updated".to_string(),
            "sonarr".to_string(),
            8989.0,
            "abc123".to_string(),
            false,
            1.0,
            "Any".to_string(),
            "/tv".to_string(),
            false,
            true,
            true,
        );

        let client = OverseerrClient::new(&server.uri(), "test-key");
        let result = client.update_sonarr(2, settings).await.unwrap();
        assert_eq!(result.name, "Sonarr Main");
    }

    #[tokio::test]
    async fn update_radarr_returns_settings() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/settings/radarr/4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(make_radarr_settings_json()))
            .mount(&server)
            .await;

        let settings = overseerr::models::RadarrSettings::new(
            "Radarr Updated".to_string(),
            "radarr".to_string(),
            7878.0,
            "xyz789".to_string(),
            false,
            1.0,
            "Any".to_string(),
            "/movies".to_string(),
            false,
            "released".to_string(),
            true,
        );

        let client = OverseerrClient::new(&server.uri(), "test-key");
        let result = client.update_radarr(4, settings).await.unwrap();
        assert_eq!(result.name, "Radarr Main");
    }
}

// ---------------------------------------------------------------------------
// ProwlarrClient integration tests via wiremock
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// k8s::read_secret_key tests
// ---------------------------------------------------------------------------

mod k8s_secret {
    use super::*;
    use kube::config::{
        AuthInfo, Cluster, Context as KubeContext, KubeConfigOptions, Kubeconfig, NamedAuthInfo,
        NamedCluster, NamedContext,
    };

    async fn mock_client(server_uri: &str) -> kube::Client {
        let kubeconfig = Kubeconfig {
            clusters: vec![NamedCluster {
                name: "test".into(),
                cluster: Some(Cluster {
                    server: Some(server_uri.to_string()),
                    insecure_skip_tls_verify: Some(true),
                    ..Default::default()
                }),
            }],
            contexts: vec![NamedContext {
                name: "test".into(),
                context: Some(KubeContext {
                    cluster: "test".into(),
                    user: Some("test".into()),
                    namespace: Some("test".into()),
                    ..Default::default()
                }),
            }],
            auth_infos: vec![NamedAuthInfo {
                name: "test".into(),
                auth_info: Some(AuthInfo::default()),
            }],
            current_context: Some("test".into()),
            ..Default::default()
        };

        let config =
            kube::Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
                .await
                .unwrap();
        kube::Client::try_from(config).unwrap()
    }

    #[tokio::test]
    async fn test_read_secret_key_success() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/my-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": { "name": "my-secret", "namespace": "test" },
                "data": { "api-key": "dGVzdC1rZXk=" }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server.uri()).await;
        let result = servarr_api::read_secret_key(&client, "test", "my-secret", "api-key").await;
        assert!(result.is_ok(), "should succeed, got: {result:?}");
        assert_eq!(result.unwrap(), "test-key");
    }

    #[tokio::test]
    async fn test_read_secret_key_not_found() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/my-secret"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {},
                "status": "Failure",
                "message": "secrets \"my-secret\" not found",
                "reason": "NotFound",
                "code": 404
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server.uri()).await;
        let result = servarr_api::read_secret_key(&client, "test", "my-secret", "api-key").await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), SecretError::Kube(_)),
            "should be a Kube error variant"
        );
    }

    #[tokio::test]
    async fn test_read_secret_key_no_data() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/my-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": { "name": "my-secret", "namespace": "test" }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server.uri()).await;
        let result = servarr_api::read_secret_key(&client, "test", "my-secret", "api-key").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SecretError::NoData { name } => {
                assert_eq!(name, "my-secret");
            }
            other => panic!("expected NoData, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_read_secret_key_missing_key() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/my-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": { "name": "my-secret", "namespace": "test" },
                "data": { "other-key": "c29tZS12YWx1ZQ==" }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server.uri()).await;
        let result = servarr_api::read_secret_key(&client, "test", "my-secret", "api-key").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SecretError::KeyNotFound { name, key } => {
                assert_eq!(name, "my-secret");
                assert_eq!(key, "api-key");
            }
            other => panic!("expected KeyNotFound, got: {other}"),
        }
    }
}

mod prowlarr_client {
    use super::*;

    fn make_application_json(id: i32) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": "Sonarr",
            "syncLevel": "fullSync",
            "implementation": "Sonarr",
            "configContract": "SonarrSettings",
            "fields": [
                {"name": "baseUrl", "value": "http://sonarr:8989"},
                {"name": "apiKey", "value": "abc123"}
            ],
            "tags": [1, 2]
        })
    }

    #[tokio::test]
    async fn list_applications_returns_array() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v1/applications"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                make_application_json(1),
                make_application_json(2)
            ])))
            .mount(&server)
            .await;

        let client = ProwlarrClient::new(&server.uri(), "test-key").unwrap();
        let apps = client.list_applications().await.unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].id, 1);
        assert_eq!(apps[0].name, "Sonarr");
        assert_eq!(apps[0].sync_level, "fullSync");
        assert_eq!(apps[1].id, 2);
    }

    #[tokio::test]
    async fn add_application_returns_ok() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/applications"))
            .respond_with(ResponseTemplate::new(201).set_body_json(make_application_json(10)))
            .mount(&server)
            .await;

        let client = ProwlarrClient::new(&server.uri(), "test-key").unwrap();
        let app = servarr_api::prowlarr::ProwlarrApp {
            id: 0,
            name: "Sonarr".to_string(),
            sync_level: "fullSync".to_string(),
            implementation: "Sonarr".to_string(),
            config_contract: "SonarrSettings".to_string(),
            fields: vec![servarr_api::prowlarr::ProwlarrAppField {
                name: "baseUrl".to_string(),
                value: serde_json::json!("http://sonarr:8989"),
            }],
            tags: vec![1, 2],
        };
        let result = client.add_application(&app).await.unwrap();
        assert_eq!(result.id, 10);
        assert_eq!(result.name, "Sonarr");
    }

    #[tokio::test]
    async fn update_application_returns_ok() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/api/v1/applications/5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(make_application_json(5)))
            .mount(&server)
            .await;

        let client = ProwlarrClient::new(&server.uri(), "test-key").unwrap();
        let app = servarr_api::prowlarr::ProwlarrApp {
            id: 5,
            name: "Sonarr".to_string(),
            sync_level: "fullSync".to_string(),
            implementation: "Sonarr".to_string(),
            config_contract: "SonarrSettings".to_string(),
            fields: vec![],
            tags: vec![],
        };
        let result = client.update_application(5, &app).await.unwrap();
        assert_eq!(result.id, 5);
    }

    #[tokio::test]
    async fn delete_application_returns_ok() {
        let server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path("/api/v1/applications/3"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = ProwlarrClient::new(&server.uri(), "test-key").unwrap();
        let result = client.delete_application(3).await;
        assert!(result.is_ok());
    }
}
