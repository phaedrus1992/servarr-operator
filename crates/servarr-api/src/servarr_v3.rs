use serde::Deserialize;

use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Which Servarr v3 application this client targets.
///
/// Used to dispatch SDK calls to the correct crate
/// (sonarr, radarr, lidarr, or prowlarr).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    Sonarr,
    Radarr,
    Lidarr,
    Prowlarr,
}

/// Client for the Servarr v3 REST API shared by Sonarr, Radarr, Lidarr, and Prowlarr.
///
/// Internally dispatches to the devopsarr SDK crate matching [`AppKind`].
/// The `create_backup` endpoint is not present in the SDK, so it falls
/// back to a direct HTTP call.
#[derive(Debug, Clone)]
pub struct ServarrClient {
    kind: AppKind,
    sonarr_config: sonarr::apis::configuration::Configuration,
    radarr_config: radarr::apis::configuration::Configuration,
    lidarr_config: lidarr::apis::configuration::Configuration,
    prowlarr_config: prowlarr::apis::configuration::Configuration,
    /// Kept for `create_backup` which has no SDK endpoint.
    http: HttpClient,
}

// --- Response types (unchanged public API) ---

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub app_name: String,
    pub version: String,
    #[serde(default)]
    pub build_time: String,
    #[serde(default)]
    pub is_debug: bool,
    #[serde(default)]
    pub is_production: bool,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub is_user_interactive: bool,
    #[serde(default)]
    pub startup_path: String,
    #[serde(default)]
    pub app_data: String,
    #[serde(default)]
    pub os_name: String,
    #[serde(default)]
    pub os_version: String,
    #[serde(default)]
    pub runtime_name: String,
    #[serde(default)]
    pub runtime_version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckResult {
    pub source: String,
    #[serde(rename = "type")]
    pub check_type: String,
    pub message: String,
    #[serde(default)]
    pub wiki_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootFolder {
    pub id: i64,
    pub path: String,
    #[serde(default)]
    pub accessible: bool,
    #[serde(default)]
    pub free_space: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub installable: bool,
    #[serde(default)]
    pub latest: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    pub id: i64,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub time: String,
}

// --- Helper: unwrap Option<Option<String>> from SDK types ---

fn oo_str(v: &Option<Option<String>>) -> String {
    v.as_ref()
        .and_then(|inner| inner.as_deref())
        .unwrap_or("")
        .to_string()
}

fn map_sdk_err<E: std::fmt::Debug>(e: E) -> ApiError {
    ApiError::ApiResponse {
        status: 0,
        body: format!("{e:?}"),
    }
}

// --- Conversion helpers from SDK model types to our types ---

fn system_status_from_sonarr(r: sonarr::models::SystemResource) -> SystemStatus {
    SystemStatus {
        app_name: oo_str(&r.app_name),
        version: oo_str(&r.version),
        build_time: r.build_time.unwrap_or_default(),
        is_debug: r.is_debug.unwrap_or(false),
        is_production: r.is_production.unwrap_or(false),
        is_admin: r.is_admin.unwrap_or(false),
        is_user_interactive: r.is_user_interactive.unwrap_or(false),
        startup_path: oo_str(&r.startup_path),
        app_data: oo_str(&r.app_data),
        os_name: oo_str(&r.os_name),
        os_version: oo_str(&r.os_version),
        runtime_name: oo_str(&r.runtime_name),
        runtime_version: oo_str(&r.runtime_version),
    }
}

fn system_status_from_radarr(r: radarr::models::SystemResource) -> SystemStatus {
    SystemStatus {
        app_name: oo_str(&r.app_name),
        version: oo_str(&r.version),
        build_time: r.build_time.unwrap_or_default(),
        is_debug: r.is_debug.unwrap_or(false),
        is_production: r.is_production.unwrap_or(false),
        is_admin: r.is_admin.unwrap_or(false),
        is_user_interactive: r.is_user_interactive.unwrap_or(false),
        startup_path: oo_str(&r.startup_path),
        app_data: oo_str(&r.app_data),
        os_name: oo_str(&r.os_name),
        os_version: oo_str(&r.os_version),
        runtime_name: oo_str(&r.runtime_name),
        runtime_version: oo_str(&r.runtime_version),
    }
}

fn system_status_from_lidarr(r: lidarr::models::SystemResource) -> SystemStatus {
    SystemStatus {
        app_name: oo_str(&r.app_name),
        version: oo_str(&r.version),
        build_time: r.build_time.unwrap_or_default(),
        is_debug: r.is_debug.unwrap_or(false),
        is_production: r.is_production.unwrap_or(false),
        is_admin: r.is_admin.unwrap_or(false),
        is_user_interactive: r.is_user_interactive.unwrap_or(false),
        startup_path: oo_str(&r.startup_path),
        app_data: oo_str(&r.app_data),
        os_name: oo_str(&r.os_name),
        os_version: oo_str(&r.os_version),
        runtime_name: oo_str(&r.runtime_name),
        runtime_version: oo_str(&r.runtime_version),
    }
}

fn system_status_from_prowlarr(r: prowlarr::models::SystemResource) -> SystemStatus {
    SystemStatus {
        app_name: oo_str(&r.app_name),
        version: oo_str(&r.version),
        build_time: r.build_time.unwrap_or_default(),
        is_debug: r.is_debug.unwrap_or(false),
        is_production: r.is_production.unwrap_or(false),
        is_admin: r.is_admin.unwrap_or(false),
        is_user_interactive: r.is_user_interactive.unwrap_or(false),
        startup_path: oo_str(&r.startup_path),
        app_data: oo_str(&r.app_data),
        os_name: oo_str(&r.os_name),
        os_version: oo_str(&r.os_version),
        runtime_name: oo_str(&r.runtime_name),
        runtime_version: oo_str(&r.runtime_version),
    }
}

fn health_from_sonarr(items: Vec<sonarr::models::HealthResource>) -> Vec<HealthCheckResult> {
    items
        .into_iter()
        .map(|h| HealthCheckResult {
            source: oo_str(&h.source),
            check_type: h.r#type.map(|t| t.to_string()).unwrap_or_default(),
            message: oo_str(&h.message),
            wiki_url: h.wiki_url.unwrap_or_default(),
        })
        .collect()
}

macro_rules! health_from_oo {
    ($mod:ident, $items:expr) => {
        $items
            .into_iter()
            .map(|h: $mod::models::HealthResource| HealthCheckResult {
                source: oo_str(&h.source),
                check_type: h.r#type.map(|t| t.to_string()).unwrap_or_default(),
                message: oo_str(&h.message),
                wiki_url: oo_str(&h.wiki_url),
            })
            .collect()
    };
}

macro_rules! root_folder_from {
    ($mod:ident, $items:expr) => {
        $items
            .into_iter()
            .map(|r: $mod::models::RootFolderResource| RootFolder {
                id: r.id.unwrap_or(0) as i64,
                path: oo_str(&r.path),
                accessible: r.accessible.unwrap_or(false),
                free_space: r.free_space.and_then(|v| v).unwrap_or(0),
            })
            .collect()
    };
}

macro_rules! update_from {
    ($mod:ident, $items:expr) => {
        $items
            .into_iter()
            .map(|u: $mod::models::UpdateResource| UpdateInfo {
                version: oo_str(&u.version),
                installed: u.installed.unwrap_or(false),
                installable: u.installable.unwrap_or(false),
                latest: u.latest.unwrap_or(false),
            })
            .collect()
    };
}

macro_rules! backup_from {
    ($mod:ident, $items:expr) => {
        $items
            .into_iter()
            .map(|b: $mod::models::BackupResource| Backup {
                id: b.id.unwrap_or(0) as i64,
                name: oo_str(&b.name),
                path: oo_str(&b.path),
                size: b.size.unwrap_or(0),
                time: b.time.unwrap_or_default(),
            })
            .collect()
    };
}

impl ServarrClient {
    /// Create a new Servarr v3 API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://sonarr:8989`).
    /// `app_kind` selects which SDK crate to use for API calls.
    pub fn new(base_url: &str, api_key: &str, app_kind: AppKind) -> Result<Self, ApiError> {
        let base = base_url.trim_end_matches('/').to_string();
        let http_url = format!("{base}/api/v3/");

        let mut sonarr_config = sonarr::apis::configuration::Configuration::new();
        sonarr_config.base_path = base.clone();
        sonarr_config.api_key = Some(sonarr::apis::configuration::ApiKey {
            prefix: None,
            key: api_key.to_string(),
        });

        let mut radarr_config = radarr::apis::configuration::Configuration::new();
        radarr_config.base_path = base.clone();
        radarr_config.api_key = Some(radarr::apis::configuration::ApiKey {
            prefix: None,
            key: api_key.to_string(),
        });

        let mut lidarr_config = lidarr::apis::configuration::Configuration::new();
        lidarr_config.base_path = base.clone();
        lidarr_config.api_key = Some(lidarr::apis::configuration::ApiKey {
            prefix: None,
            key: api_key.to_string(),
        });

        let mut prowlarr_config = prowlarr::apis::configuration::Configuration::new();
        prowlarr_config.base_path = base;
        prowlarr_config.api_key = Some(prowlarr::apis::configuration::ApiKey {
            prefix: None,
            key: api_key.to_string(),
        });

        Ok(Self {
            kind: app_kind,
            sonarr_config,
            radarr_config,
            lidarr_config,
            prowlarr_config,
            http: HttpClient::new(&http_url, Some(api_key))?,
        })
    }

    /// GET `/api/v3/system/status`
    pub async fn system_status(&self) -> Result<SystemStatus, ApiError> {
        match self.kind {
            AppKind::Sonarr => sonarr::apis::system_api::get_system_status(&self.sonarr_config)
                .await
                .map(system_status_from_sonarr)
                .map_err(map_sdk_err),
            AppKind::Radarr => radarr::apis::system_api::get_system_status(&self.radarr_config)
                .await
                .map(system_status_from_radarr)
                .map_err(map_sdk_err),
            AppKind::Lidarr => lidarr::apis::system_api::get_system_status(&self.lidarr_config)
                .await
                .map(system_status_from_lidarr)
                .map_err(map_sdk_err),
            AppKind::Prowlarr => {
                prowlarr::apis::system_api::get_system_status(&self.prowlarr_config)
                    .await
                    .map(system_status_from_prowlarr)
                    .map_err(map_sdk_err)
            }
        }
    }

    /// GET `/api/v3/health`
    pub async fn health(&self) -> Result<Vec<HealthCheckResult>, ApiError> {
        match self.kind {
            AppKind::Sonarr => sonarr::apis::health_api::list_health(&self.sonarr_config)
                .await
                .map(health_from_sonarr)
                .map_err(map_sdk_err),
            AppKind::Radarr => radarr::apis::health_api::list_health(&self.radarr_config)
                .await
                .map(|v| health_from_oo!(radarr, v))
                .map_err(map_sdk_err),
            AppKind::Lidarr => lidarr::apis::health_api::list_health(&self.lidarr_config)
                .await
                .map(|v| health_from_oo!(lidarr, v))
                .map_err(map_sdk_err),
            AppKind::Prowlarr => prowlarr::apis::health_api::list_health(&self.prowlarr_config)
                .await
                .map(|v| health_from_oo!(prowlarr, v))
                .map_err(map_sdk_err),
        }
    }

    /// GET `/api/v3/rootfolder`
    pub async fn root_folder(&self) -> Result<Vec<RootFolder>, ApiError> {
        match self.kind {
            AppKind::Sonarr => sonarr::apis::root_folder_api::list_root_folder(&self.sonarr_config)
                .await
                .map(|v| root_folder_from!(sonarr, v))
                .map_err(map_sdk_err),
            AppKind::Radarr => radarr::apis::root_folder_api::list_root_folder(&self.radarr_config)
                .await
                .map(|v| root_folder_from!(radarr, v))
                .map_err(map_sdk_err),
            AppKind::Lidarr => lidarr::apis::root_folder_api::list_root_folder(&self.lidarr_config)
                .await
                .map(|v| root_folder_from!(lidarr, v))
                .map_err(map_sdk_err),
            // Prowlarr does not have a root folder API
            AppKind::Prowlarr => Ok(Vec::new()),
        }
    }

    /// GET `/api/v3/update` — returns available updates.
    pub async fn updates(&self) -> Result<Vec<UpdateInfo>, ApiError> {
        match self.kind {
            AppKind::Sonarr => sonarr::apis::update_api::list_update(&self.sonarr_config)
                .await
                .map(|v| update_from!(sonarr, v))
                .map_err(map_sdk_err),
            AppKind::Radarr => radarr::apis::update_api::list_update(&self.radarr_config)
                .await
                .map(|v| update_from!(radarr, v))
                .map_err(map_sdk_err),
            AppKind::Lidarr => lidarr::apis::update_api::list_update(&self.lidarr_config)
                .await
                .map(|v| update_from!(lidarr, v))
                .map_err(map_sdk_err),
            AppKind::Prowlarr => prowlarr::apis::update_api::list_update(&self.prowlarr_config)
                .await
                .map(|v| update_from!(prowlarr, v))
                .map_err(map_sdk_err),
        }
    }

    /// GET `/api/v3/system/backup` — list all backups.
    pub async fn list_backups(&self) -> Result<Vec<Backup>, ApiError> {
        match self.kind {
            AppKind::Sonarr => sonarr::apis::backup_api::list_system_backup(&self.sonarr_config)
                .await
                .map(|v| backup_from!(sonarr, v))
                .map_err(map_sdk_err),
            AppKind::Radarr => radarr::apis::backup_api::list_system_backup(&self.radarr_config)
                .await
                .map(|v| backup_from!(radarr, v))
                .map_err(map_sdk_err),
            AppKind::Lidarr => lidarr::apis::backup_api::list_system_backup(&self.lidarr_config)
                .await
                .map(|v| backup_from!(lidarr, v))
                .map_err(map_sdk_err),
            AppKind::Prowlarr => {
                prowlarr::apis::backup_api::list_system_backup(&self.prowlarr_config)
                    .await
                    .map(|v| backup_from!(prowlarr, v))
                    .map_err(map_sdk_err)
            }
        }
    }

    /// POST `/api/v3/system/backup` — create a new backup.
    ///
    /// The SDK crates do not expose a create-backup endpoint, so this
    /// falls back to a direct HTTP POST.
    pub async fn create_backup(&self) -> Result<Backup, ApiError> {
        self.http
            .post("system/backup", &serde_json::json!({}))
            .await
    }

    /// POST `/api/v3/system/backup/restore/{id}` — restore from a backup.
    pub async fn restore_backup(&self, id: i64) -> Result<(), ApiError> {
        let id32 = id as i32;
        match self.kind {
            AppKind::Sonarr => sonarr::apis::backup_api::create_system_backup_restore_by_id(
                &self.sonarr_config,
                id32,
            )
            .await
            .map_err(map_sdk_err),
            AppKind::Radarr => radarr::apis::backup_api::create_system_backup_restore_by_id(
                &self.radarr_config,
                id32,
            )
            .await
            .map_err(map_sdk_err),
            AppKind::Lidarr => lidarr::apis::backup_api::create_system_backup_restore_by_id(
                &self.lidarr_config,
                id32,
            )
            .await
            .map_err(map_sdk_err),
            AppKind::Prowlarr => prowlarr::apis::backup_api::create_system_backup_restore_by_id(
                &self.prowlarr_config,
                id32,
            )
            .await
            .map_err(map_sdk_err),
        }
    }

    /// DELETE `/api/v3/system/backup/{id}` — delete a backup.
    pub async fn delete_backup(&self, id: i64) -> Result<(), ApiError> {
        let id32 = id as i32;
        match self.kind {
            AppKind::Sonarr => {
                sonarr::apis::backup_api::delete_system_backup(&self.sonarr_config, id32)
                    .await
                    .map_err(map_sdk_err)
            }
            AppKind::Radarr => {
                radarr::apis::backup_api::delete_system_backup(&self.radarr_config, id32)
                    .await
                    .map_err(map_sdk_err)
            }
            AppKind::Lidarr => {
                lidarr::apis::backup_api::delete_system_backup(&self.lidarr_config, id32)
                    .await
                    .map_err(map_sdk_err)
            }
            AppKind::Prowlarr => {
                prowlarr::apis::backup_api::delete_system_backup(&self.prowlarr_config, id32)
                    .await
                    .map_err(map_sdk_err)
            }
        }
    }

    /// Configure Forms authentication credentials via `PUT /api/v3/config/host`.
    ///
    /// Fetches the current host configuration, sets `authenticationMethod` to
    /// `"forms"`, and injects the username and password.  Sonarr/Radarr/Lidarr/
    /// Prowlarr BCrypt-hash the password before storage, so plaintext is safe to
    /// pass here.
    ///
    /// Safe to call when authentication is currently disabled or the app is in
    /// first-run setup mode (no users yet).
    ///
    /// Returns `Err(ApiError::ApiResponse { status: 401, .. })` when auth is
    /// already enabled and the client has no valid API key.  The caller should
    /// treat that as "credentials already configured" rather than a fatal error.
    pub async fn configure_admin(&self, username: &str, password: &str) -> Result<(), ApiError> {
        let mut config: serde_json::Value = self.http.get("config/host").await?;
        let id = config.get("id").and_then(|v| v.as_i64()).unwrap_or(1);
        config["authenticationMethod"] = serde_json::json!("forms");
        config["username"] = serde_json::json!(username);
        config["password"] = serde_json::json!(password);
        config["passwordConfirmation"] = serde_json::json!(password);
        let _: serde_json::Value = self.http.put(&format!("config/host/{id}"), &config).await?;
        Ok(())
    }
}

impl HealthCheck for ServarrClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let status = self.system_status().await?;
        Ok(!status.version.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oo_str_none() {
        assert_eq!(oo_str(&None), "");
    }

    #[test]
    fn oo_str_some_none() {
        assert_eq!(oo_str(&Some(None)), "");
    }

    #[test]
    fn oo_str_some_some_value() {
        let v = Some(Some("hello".to_string()));
        assert_eq!(oo_str(&v), "hello");
    }

    #[test]
    fn oo_str_some_some_empty() {
        let v = Some(Some(String::new()));
        assert_eq!(oo_str(&v), "");
    }

    #[test]
    fn map_sdk_err_formats_debug() {
        let err = map_sdk_err("something went wrong");
        match err {
            ApiError::ApiResponse { status, body } => {
                assert_eq!(status, 0);
                assert_eq!(body, "\"something went wrong\"");
            }
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[test]
    fn new_sonarr_client() {
        let client = ServarrClient::new("http://localhost:8989", "test-key", AppKind::Sonarr);
        assert!(client.is_ok());
    }

    #[test]
    fn new_radarr_client() {
        let client = ServarrClient::new("http://localhost:7878", "test-key", AppKind::Radarr);
        assert!(client.is_ok());
    }

    #[test]
    fn new_lidarr_client() {
        let client = ServarrClient::new("http://localhost:8686", "test-key", AppKind::Lidarr);
        assert!(client.is_ok());
    }

    #[test]
    fn new_prowlarr_client() {
        let client = ServarrClient::new("http://localhost:9696", "test-key", AppKind::Prowlarr);
        assert!(client.is_ok());
    }

    #[test]
    fn new_with_invalid_url_returns_error() {
        let result = ServarrClient::new("not a url", "key", AppKind::Sonarr);
        assert!(result.is_err());
    }

    // -- Conversion helper unit tests -----------------------------------------

    fn make_oo(s: &str) -> Option<Option<String>> {
        Some(Some(s.to_string()))
    }

    #[test]
    fn system_status_from_radarr_converts_fields() {
        let r = radarr::models::SystemResource {
            app_name: make_oo("Radarr"),
            version: make_oo("5.0.0"),
            is_production: Some(true),
            os_name: make_oo("Linux"),
            ..Default::default()
        };

        let s = system_status_from_radarr(r);
        assert_eq!(s.app_name, "Radarr");
        assert_eq!(s.version, "5.0.0");
        assert!(s.is_production);
        assert_eq!(s.os_name, "Linux");
    }

    #[test]
    fn system_status_from_lidarr_converts_fields() {
        let r = lidarr::models::SystemResource {
            app_name: make_oo("Lidarr"),
            version: make_oo("2.0.0"),
            is_debug: Some(true),
            ..Default::default()
        };

        let s = system_status_from_lidarr(r);
        assert_eq!(s.app_name, "Lidarr");
        assert_eq!(s.version, "2.0.0");
        assert!(s.is_debug);
    }

    #[test]
    fn system_status_from_prowlarr_converts_fields() {
        let r = prowlarr::models::SystemResource {
            app_name: make_oo("Prowlarr"),
            version: make_oo("1.5.0"),
            runtime_name: make_oo(".NET"),
            ..Default::default()
        };

        let s = system_status_from_prowlarr(r);
        assert_eq!(s.app_name, "Prowlarr");
        assert_eq!(s.version, "1.5.0");
        assert_eq!(s.runtime_name, ".NET");
    }

    #[test]
    fn system_status_from_sonarr_converts_fields() {
        let r = sonarr::models::SystemResource {
            app_name: make_oo("Sonarr"),
            version: make_oo("4.0.0"),
            startup_path: make_oo("/opt/sonarr"),
            ..Default::default()
        };

        let s = system_status_from_sonarr(r);
        assert_eq!(s.app_name, "Sonarr");
        assert_eq!(s.version, "4.0.0");
        assert_eq!(s.startup_path, "/opt/sonarr");
    }

    #[test]
    fn system_status_defaults_for_missing_fields() {
        let r = radarr::models::SystemResource::default();
        let s = system_status_from_radarr(r);
        assert_eq!(s.app_name, "");
        assert_eq!(s.version, "");
        assert!(!s.is_debug);
        assert!(!s.is_production);
        assert_eq!(s.os_name, "");
    }

    #[test]
    fn health_from_sonarr_converts_items() {
        let h = sonarr::models::HealthResource {
            source: make_oo("IndexerCheck"),
            r#type: Some(sonarr::models::HealthCheckResult::Ok),
            message: make_oo("All good"),
            wiki_url: Some("https://wiki.example.com".to_string()),
            ..Default::default()
        };
        let result = health_from_sonarr(vec![h]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, "IndexerCheck");
        assert_eq!(result[0].message, "All good");
    }

    #[test]
    fn health_from_radarr_via_macro() {
        let h = radarr::models::HealthResource {
            source: make_oo("DiskCheck"),
            r#type: Some(radarr::models::HealthCheckResult::Warning),
            message: make_oo("Low space"),
            wiki_url: make_oo("https://wiki.example.com"),
            ..Default::default()
        };
        let result: Vec<HealthCheckResult> = health_from_oo!(radarr, vec![h]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, "DiskCheck");
        assert_eq!(result[0].message, "Low space");
    }

    #[test]
    fn health_from_lidarr_via_macro() {
        let h = lidarr::models::HealthResource {
            source: make_oo("UpdateCheck"),
            r#type: Some(lidarr::models::HealthCheckResult::Ok),
            message: make_oo("Up to date"),
            wiki_url: make_oo(""),
            ..Default::default()
        };
        let result: Vec<HealthCheckResult> = health_from_oo!(lidarr, vec![h]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, "UpdateCheck");
    }

    #[test]
    fn health_from_prowlarr_via_macro() {
        let h = prowlarr::models::HealthResource {
            source: make_oo("IndexerSync"),
            r#type: Some(prowlarr::models::HealthCheckResult::Ok),
            message: make_oo("Synced"),
            wiki_url: make_oo(""),
            ..Default::default()
        };
        let result: Vec<HealthCheckResult> = health_from_oo!(prowlarr, vec![h]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, "IndexerSync");
    }
}
