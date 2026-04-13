use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use axum_server::tls_rustls::RustlsConfig;
use kube::Client;
use kube::api::{Api, ListParams};
use serde::{Deserialize, Serialize};
use servarr_crds::{AppConfig, AppType, ServarrApp, ServarrAppSpec, SshMode};
use tracing::{debug, info, warn};

const DEFAULT_WEBHOOK_PORT: u16 = 9443;

const DEFAULT_TLS_DIR: &str = "/etc/webhook/tls";

/// Configuration for the webhook server.
#[derive(Clone)]
pub struct WebhookConfig {
    pub port: u16,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        let port = match std::env::var("WEBHOOK_PORT") {
            Ok(s) => match s.parse::<u16>() {
                Ok(p) => {
                    debug!(port = p, "using WEBHOOK_PORT from env");
                    p
                }
                Err(e) => {
                    warn!(value = %s, error = %e, "invalid WEBHOOK_PORT, using default {DEFAULT_WEBHOOK_PORT}");
                    DEFAULT_WEBHOOK_PORT
                }
            },
            Err(_) => DEFAULT_WEBHOOK_PORT,
        };

        let tls_dir =
            std::env::var("WEBHOOK_TLS_DIR").unwrap_or_else(|_| DEFAULT_TLS_DIR.to_string());
        let tls_cert = std::env::var("WEBHOOK_TLS_CERT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(&tls_dir).join("tls.crt"));
        let tls_key = std::env::var("WEBHOOK_TLS_KEY")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(&tls_dir).join("tls.key"));

        Self {
            port,
            tls_cert,
            tls_key,
        }
    }
}

#[derive(Clone)]
struct WebhookState {
    client: Client,
}

// --- Admission API types ---

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionReview {
    api_version: String,
    kind: String,
    request: Option<AdmissionRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionRequest {
    uid: String,
    #[serde(default)]
    operation: String,
    #[serde(default)]
    namespace: String,
    object: serde_json::Value,
    #[serde(default)]
    old_object: Option<serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionReviewResponse {
    api_version: String,
    kind: String,
    response: AdmissionResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionResponse {
    uid: String,
    allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<AdmissionStatus>,
}

#[derive(Serialize)]
struct AdmissionStatus {
    message: String,
}

/// Start the validating webhook server.
///
/// Listens for `POST /validate-servarrapp` with AdmissionReview payloads.
/// Serves TLS using the cert/key at `config.tls_cert` / `config.tls_key`
/// (defaults: `/etc/webhook/tls/tls.crt` and `/etc/webhook/tls/tls.key`).
/// Override paths via `WEBHOOK_TLS_CERT`, `WEBHOOK_TLS_KEY`, or `WEBHOOK_TLS_DIR`.
/// Set `WEBHOOK_PORT` to override the default port 9443.
pub async fn run(client: kube::Client, config: WebhookConfig) -> anyhow::Result<()> {
    let state = Arc::new(WebhookState { client });
    let app = Router::new()
        .route("/validate-servarrapp", post(validate_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!(%addr, cert = %config.tls_cert.display(), "starting webhook server (TLS)");

    let tls = RustlsConfig::from_pem_file(&config.tls_cert, &config.tls_key)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to load webhook TLS cert {:?} / key {:?}: {e}",
                config.tls_cert,
                config.tls_key
            )
        })?;

    axum_server::bind_rustls(addr, tls)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

async fn validate_handler(
    State(_state): State<Arc<WebhookState>>,
    Json(review): Json<AdmissionReview>,
) -> impl IntoResponse {
    let request = match review.request {
        Some(req) => req,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing request"})),
            );
        }
    };

    let uid = request.uid.clone();
    let validation_result = validate_spec(
        &request.object,
        request.old_object.as_ref(),
        &request.operation,
        &request.namespace,
        &_state.client,
    )
    .await;

    let response = AdmissionReviewResponse {
        api_version: review.api_version,
        kind: review.kind,
        response: match validation_result {
            Ok(()) => AdmissionResponse {
                uid,
                allowed: true,
                status: None,
            },
            Err(msg) => {
                warn!(%msg, "admission rejected");
                AdmissionResponse {
                    uid,
                    allowed: false,
                    status: Some(AdmissionStatus { message: msg }),
                }
            }
        },
    };

    (
        StatusCode::OK,
        Json(serde_json::to_value(response).unwrap()),
    )
}

/// Validate a ServarrApp spec. Returns `Ok(())` on success or `Err(message)`.
async fn validate_spec(
    object: &serde_json::Value,
    old_object: Option<&serde_json::Value>,
    operation: &str,
    namespace: &str,
    client: &Client,
) -> Result<(), String> {
    let spec = object
        .get("spec")
        .ok_or_else(|| "missing spec field".to_string())?;

    let parsed: ServarrAppSpec =
        serde_json::from_value(spec.clone()).map_err(|e| format!("invalid spec: {e}"))?;

    debug!(
        operation,
        namespace,
        app = %parsed.app,
        instance = ?parsed.instance,
        "validating ServarrApp admission"
    );

    let mut errors = Vec::new();

    // Rule 1: AppConfig variant must match AppType
    validate_app_config_match(&parsed, &mut errors);

    // Rule 2: Port numbers must be in range 1-65535
    validate_port_ranges(&parsed, &mut errors);

    // Rule 3: Resource limits >= requests
    validate_resource_bounds(&parsed, &mut errors);

    // Rule 4: gateway.hosts must be non-empty when gateway.enabled
    validate_gateway_hosts(&parsed, &mut errors);

    // Rule 5: Volume names in persistence must be unique
    validate_unique_volume_names(&parsed, &mut errors);

    // Rule 6: Duplicate app+instance detection on CREATE
    if operation == "CREATE" && !namespace.is_empty() {
        validate_no_duplicate_instance(&parsed, namespace, client, &mut errors).await;
    }

    // Rule 6b: app and instance are immutable on UPDATE
    if operation == "UPDATE" {
        validate_identity_immutable(&parsed, old_object, &mut errors);
    }

    // Rule 7: Transmission settings must not override operator-managed keys
    validate_transmission_settings(&parsed, &mut errors);

    // Rule 8: Backup retention_count must be >= 1 when backups are enabled
    validate_backup_retention(&parsed, &mut errors);

    // Rule 9: IndexerDefinition names must be alphanumeric with optional hyphens
    validate_indexer_definition_names(&parsed, &mut errors);

    // Rule 10: SSH bastion shell overrides not allowed in restricted modes
    validate_ssh_shell_override(&parsed, &mut errors);

    // Rule 11: adminCredentials.secretName must be non-empty when set
    validate_admin_credentials(&parsed, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn validate_identity_immutable(
    spec: &ServarrAppSpec,
    old_object: Option<&serde_json::Value>,
    errors: &mut Vec<String>,
) {
    let old_spec = old_object
        .and_then(|o| o.get("spec"))
        .and_then(|s| serde_json::from_value::<ServarrAppSpec>(s.clone()).ok());

    if let Some(old) = old_spec {
        if old.app != spec.app {
            debug!(
                old_app = %old.app,
                new_app = %spec.app,
                "rejecting app type change on UPDATE"
            );
            errors.push(format!(
                "spec.app is immutable (was '{}', got '{}')",
                old.app, spec.app
            ));
        }
        if old.instance != spec.instance {
            debug!(
                old_instance = ?old.instance,
                new_instance = ?spec.instance,
                "rejecting instance change on UPDATE"
            );
            errors.push(format!(
                "spec.instance is immutable (was {:?}, got {:?})",
                old.instance, spec.instance
            ));
        }
    }
}

fn validate_admin_credentials(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref ac) = spec.admin_credentials
        && ac.secret_name.is_empty()
    {
        errors.push(
            "adminCredentials.secretName must be non-empty when adminCredentials is set"
                .to_string(),
        );
    }
}

fn validate_ssh_shell_override(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(AppConfig::SshBastion(ref sc)) = spec.app_config {
        for user in &sc.users {
            if user.mode == SshMode::RestrictedRsync && user.shell.is_some() {
                debug!(
                    user = %user.name,
                    shell = ?user.shell,
                    "rejecting shell override in restricted-rsync mode"
                );
                errors.push(format!(
                    "appConfig.sshBastion.users[{}].shell cannot be overridden in restricted-rsync mode",
                    user.name
                ));
            }
        }
    }
}

fn validate_app_config_match(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref config) = spec.app_config {
        let valid = matches!(
            (&spec.app, config),
            (AppType::Transmission, AppConfig::Transmission(_))
                | (AppType::Sabnzbd, AppConfig::Sabnzbd(_))
                | (AppType::Prowlarr, AppConfig::Prowlarr(_))
                | (AppType::SshBastion, AppConfig::SshBastion(_))
                | (AppType::Overseerr, AppConfig::Overseerr(_))
        );
        if !valid {
            errors.push(format!(
                "appConfig variant does not match app type '{}'",
                spec.app
            ));
        }
    }
}

fn validate_port_ranges(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    let check_port = |port: i32, label: &str, errors: &mut Vec<String>| {
        if !(1..=65535).contains(&port) {
            errors.push(format!("{label}: port {port} out of range 1-65535"));
        }
    };

    if let Some(ref svc) = spec.service {
        for p in &svc.ports {
            check_port(p.port, &format!("service.ports[{}].port", p.name), errors);
            if let Some(cp) = p.container_port {
                check_port(
                    cp,
                    &format!("service.ports[{}].containerPort", p.name),
                    errors,
                );
            }
            if let Some(hp) = p.host_port {
                check_port(hp, &format!("service.ports[{}].hostPort", p.name), errors);
            }
        }
    }

    if let Some(AppConfig::Transmission(ref tc)) = spec.app_config
        && let Some(ref peer) = tc.peer_port
    {
        check_port(peer.port, "appConfig.transmission.peerPort.port", errors);
    }
}

fn validate_resource_bounds(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref res) = spec.resources {
        if let (Some(limit_val), Some(req_val)) =
            (parse_cpu(&res.limits.cpu), parse_cpu(&res.requests.cpu))
            && limit_val < req_val
        {
            errors.push(format!(
                "resources.limits.cpu ({}) must be >= resources.requests.cpu ({})",
                res.limits.cpu, res.requests.cpu
            ));
        }
        if let (Some(limit_val), Some(req_val)) = (
            parse_memory(&res.limits.memory),
            parse_memory(&res.requests.memory),
        ) && limit_val < req_val
        {
            errors.push(format!(
                "resources.limits.memory ({}) must be >= resources.requests.memory ({})",
                res.limits.memory, res.requests.memory
            ));
        }
    }
}

fn validate_gateway_hosts(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref gw) = spec.gateway
        && gw.enabled
        && gw.hosts.is_empty()
    {
        errors.push("gateway.hosts must be non-empty when gateway is enabled".into());
    }
}

async fn validate_no_duplicate_instance(
    spec: &ServarrAppSpec,
    namespace: &str,
    client: &Client,
    errors: &mut Vec<String>,
) {
    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let existing = match api.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "failed to list ServarrApps for duplicate check");
            errors.push(format!("failed to check for duplicate instances: {e}"));
            return;
        }
    };

    let new_app_type = spec.app.to_string();
    let new_instance = spec.instance.as_deref().unwrap_or("");

    for app in &existing {
        let existing_app_type = app.spec.app.to_string();
        let existing_instance = app.spec.instance.as_deref().unwrap_or("");

        if existing_app_type == new_app_type && existing_instance == new_instance {
            let instance_desc = if new_instance.is_empty() {
                "(default)".to_string()
            } else {
                format!("'{new_instance}'")
            };
            errors.push(format!(
                "a ServarrApp with app={new_app_type} instance={instance_desc} already exists in namespace {namespace}"
            ));
            return;
        }
    }
}

fn validate_unique_volume_names(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref persistence) = spec.persistence {
        let mut seen = HashSet::new();
        for v in &persistence.volumes {
            if !seen.insert(&v.name) {
                errors.push(format!("duplicate volume name: '{}'", v.name));
            }
        }

        let mut nfs_seen = HashSet::new();
        for nfs in &persistence.nfs_mounts {
            if !nfs_seen.insert(&nfs.name) {
                errors.push(format!("duplicate nfsMount name: '{}'", nfs.name));
            }
        }
    }
}

/// Keys in Transmission settings.json that are managed by the operator and
/// must not be overridden via the raw `settings` field.
const TRANSMISSION_MANAGED_KEYS: &[&str] = &[
    "rpc-authentication-required",
    "rpc-username",
    "rpc-password",
    "rpc-bind-address",
    "peer-port",
    "peer-port-random-on-start",
    "peer-port-random-low",
    "peer-port-random-high",
    "watch-dir",
    "watch-dir-enabled",
];

fn validate_transmission_settings(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(AppConfig::Transmission(ref tc)) = spec.app_config
        && let serde_json::Value::Object(ref map) = tc.settings
    {
        for key in TRANSMISSION_MANAGED_KEYS {
            if map.contains_key(*key) {
                errors.push(format!(
                    "appConfig.transmission.settings must not contain operator-managed key '{key}'"
                ));
            }
        }
    }
}

fn validate_backup_retention(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref backup) = spec.backup
        && backup.enabled
        && backup.retention_count == 0
    {
        errors.push("backup.retentionCount must be >= 1 when backups are enabled".into());
    }
}

fn validate_indexer_definition_names(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(AppConfig::Prowlarr(ref pc)) = spec.app_config {
        for def in &pc.custom_definitions {
            if !def
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
                || def.name.is_empty()
            {
                errors.push(format!(
                    "appConfig.prowlarr.customDefinitions[].name '{}' must be non-empty and contain only alphanumeric characters or hyphens",
                    def.name
                ));
            }
        }
    }
}

/// Parse CPU quantity to millicores for comparison.
fn parse_cpu(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    if let Some(m) = s.strip_suffix('m') {
        m.parse().ok()
    } else {
        s.parse::<f64>().ok().map(|v| (v * 1000.0) as u64)
    }
}

/// Parse memory quantity to bytes for comparison.
fn parse_memory(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    for (suffix, multiplier) in [
        ("Ti", 1024u64 * 1024 * 1024 * 1024),
        ("Gi", 1024 * 1024 * 1024),
        ("Mi", 1024 * 1024),
        ("Ki", 1024),
        ("T", 1000 * 1000 * 1000 * 1000),
        ("G", 1000 * 1000 * 1000),
        ("M", 1000 * 1000),
        ("K", 1000),
    ] {
        if let Some(num) = s.strip_suffix(suffix) {
            return num.parse::<u64>().ok().map(|v| v * multiplier);
        }
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use servarr_crds::*;

    // ── Helper to build a minimal ServarrAppSpec ──

    fn minimal_spec(app: AppType) -> ServarrAppSpec {
        ServarrAppSpec {
            app,
            ..Default::default()
        }
    }

    // ── parse_cpu ──

    #[test]
    fn parse_cpu_empty_string() {
        assert_eq!(parse_cpu(""), None);
    }

    #[test]
    fn parse_cpu_millicores() {
        assert_eq!(parse_cpu("500m"), Some(500));
    }

    #[test]
    fn parse_cpu_whole_cores() {
        assert_eq!(parse_cpu("1"), Some(1000));
    }

    #[test]
    fn parse_cpu_fractional_cores() {
        assert_eq!(parse_cpu("2.5"), Some(2500));
    }

    #[test]
    fn parse_cpu_quarter_core() {
        assert_eq!(parse_cpu("0.25"), Some(250));
    }

    #[test]
    fn parse_cpu_100m() {
        assert_eq!(parse_cpu("100m"), Some(100));
    }

    // ── parse_memory ──

    #[test]
    fn parse_memory_empty_string() {
        assert_eq!(parse_memory(""), None);
    }

    #[test]
    fn parse_memory_raw_bytes() {
        assert_eq!(parse_memory("1024"), Some(1024));
    }

    #[test]
    fn parse_memory_ki() {
        assert_eq!(parse_memory("1Ki"), Some(1024));
    }

    #[test]
    fn parse_memory_mi() {
        assert_eq!(parse_memory("1Mi"), Some(1_048_576));
    }

    #[test]
    fn parse_memory_gi() {
        assert_eq!(parse_memory("1Gi"), Some(1_073_741_824));
    }

    #[test]
    fn parse_memory_ti() {
        assert_eq!(parse_memory("1Ti"), Some(1_099_511_627_776));
    }

    #[test]
    fn parse_memory_k_decimal() {
        assert_eq!(parse_memory("1K"), Some(1_000));
    }

    #[test]
    fn parse_memory_m_decimal() {
        assert_eq!(parse_memory("1M"), Some(1_000_000));
    }

    #[test]
    fn parse_memory_g_decimal() {
        assert_eq!(parse_memory("1G"), Some(1_000_000_000));
    }

    #[test]
    fn parse_memory_t_decimal() {
        assert_eq!(parse_memory("1T"), Some(1_000_000_000_000));
    }

    #[test]
    fn parse_memory_512mi() {
        assert_eq!(parse_memory("512Mi"), Some(536_870_912));
    }

    // ── validate_app_config_match ──

    #[test]
    fn app_config_match_no_config() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_app_config_match(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn app_config_match_transmission_ok() {
        let mut spec = minimal_spec(AppType::Transmission);
        spec.app_config = Some(AppConfig::Transmission(TransmissionConfig::default()));
        let mut errors = Vec::new();
        validate_app_config_match(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn app_config_match_sabnzbd_ok() {
        let mut spec = minimal_spec(AppType::Sabnzbd);
        spec.app_config = Some(AppConfig::Sabnzbd(SabnzbdConfig::default()));
        let mut errors = Vec::new();
        validate_app_config_match(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn app_config_match_prowlarr_ok() {
        let mut spec = minimal_spec(AppType::Prowlarr);
        spec.app_config = Some(AppConfig::Prowlarr(ProwlarrConfig::default()));
        let mut errors = Vec::new();
        validate_app_config_match(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn app_config_match_overseerr_ok() {
        let mut spec = minimal_spec(AppType::Overseerr);
        spec.app_config = Some(AppConfig::Overseerr(Box::default()));
        let mut errors = Vec::new();
        validate_app_config_match(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn app_config_match_ssh_bastion_ok() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        let mut errors = Vec::new();
        validate_app_config_match(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn app_config_match_mismatch() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.app_config = Some(AppConfig::Transmission(TransmissionConfig::default()));
        let mut errors = Vec::new();
        validate_app_config_match(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("does not match app type"));
    }

    // ── validate_port_ranges ──

    #[test]
    fn port_ranges_valid_port() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.service = Some(ServiceSpec {
            ports: vec![ServicePort {
                name: "http".into(),
                port: 8080,
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_port_ranges(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn port_ranges_port_zero() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.service = Some(ServiceSpec {
            ports: vec![ServicePort {
                name: "http".into(),
                port: 0,
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_port_ranges(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("out of range"));
    }

    #[test]
    fn port_ranges_port_65536() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.service = Some(ServiceSpec {
            ports: vec![ServicePort {
                name: "http".into(),
                port: 65536,
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_port_ranges(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("out of range"));
    }

    #[test]
    fn port_ranges_container_port_out_of_range() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.service = Some(ServiceSpec {
            ports: vec![ServicePort {
                name: "http".into(),
                port: 80,
                container_port: Some(70000),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_port_ranges(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("containerPort"));
    }

    #[test]
    fn port_ranges_host_port_out_of_range() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.service = Some(ServiceSpec {
            ports: vec![ServicePort {
                name: "http".into(),
                port: 80,
                host_port: Some(-1),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_port_ranges(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("hostPort"));
    }

    #[test]
    fn port_ranges_transmission_peer_port_out_of_range() {
        let mut spec = minimal_spec(AppType::Transmission);
        spec.app_config = Some(AppConfig::Transmission(TransmissionConfig {
            peer_port: Some(PeerPortConfig {
                port: 0,
                ..Default::default()
            }),
            ..Default::default()
        }));
        let mut errors = Vec::new();
        validate_port_ranges(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("peerPort"));
    }

    // ── validate_resource_bounds ──

    #[test]
    fn resource_bounds_no_resources() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_resource_bounds(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn resource_bounds_cpu_limit_gte_request() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.resources = Some(ResourceRequirements {
            limits: ResourceList {
                cpu: "1".into(),
                memory: "".into(),
            },
            requests: ResourceList {
                cpu: "500m".into(),
                memory: "".into(),
            },
        });
        let mut errors = Vec::new();
        validate_resource_bounds(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn resource_bounds_cpu_limit_lt_request() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.resources = Some(ResourceRequirements {
            limits: ResourceList {
                cpu: "250m".into(),
                memory: "".into(),
            },
            requests: ResourceList {
                cpu: "500m".into(),
                memory: "".into(),
            },
        });
        let mut errors = Vec::new();
        validate_resource_bounds(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("limits.cpu"));
    }

    #[test]
    fn resource_bounds_memory_limit_lt_request() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.resources = Some(ResourceRequirements {
            limits: ResourceList {
                cpu: "".into(),
                memory: "256Mi".into(),
            },
            requests: ResourceList {
                cpu: "".into(),
                memory: "512Mi".into(),
            },
        });
        let mut errors = Vec::new();
        validate_resource_bounds(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("limits.memory"));
    }

    #[test]
    fn resource_bounds_empty_cpu_no_error() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.resources = Some(ResourceRequirements {
            limits: ResourceList {
                cpu: "".into(),
                memory: "".into(),
            },
            requests: ResourceList {
                cpu: "".into(),
                memory: "".into(),
            },
        });
        let mut errors = Vec::new();
        validate_resource_bounds(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    // ── validate_gateway_hosts ──

    #[test]
    fn gateway_hosts_disabled() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.gateway = Some(GatewaySpec {
            enabled: false,
            hosts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_gateway_hosts(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn gateway_hosts_enabled_with_hosts() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.gateway = Some(GatewaySpec {
            enabled: true,
            hosts: vec!["sonarr.example.com".into()],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_gateway_hosts(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn gateway_hosts_enabled_empty_hosts() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.gateway = Some(GatewaySpec {
            enabled: true,
            hosts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_gateway_hosts(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("non-empty"));
    }

    // ── validate_unique_volume_names ──

    #[test]
    fn unique_volume_names_ok() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![
                PvcVolume {
                    name: "config".into(),
                    mount_path: "/config".into(),
                    ..Default::default()
                },
                PvcVolume {
                    name: "data".into(),
                    mount_path: "/data".into(),
                    ..Default::default()
                },
            ],
            nfs_mounts: vec![],
        });
        let mut errors = Vec::new();
        validate_unique_volume_names(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn unique_volume_names_duplicate() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![
                PvcVolume {
                    name: "config".into(),
                    mount_path: "/config".into(),
                    ..Default::default()
                },
                PvcVolume {
                    name: "config".into(),
                    mount_path: "/config2".into(),
                    ..Default::default()
                },
            ],
            nfs_mounts: vec![],
        });
        let mut errors = Vec::new();
        validate_unique_volume_names(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate volume name"));
    }

    #[test]
    fn unique_volume_names_duplicate_nfs() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![],
            nfs_mounts: vec![
                NfsMount {
                    name: "media".into(),
                    server: "nas".into(),
                    path: "/media".into(),
                    mount_path: "/media".into(),
                    ..Default::default()
                },
                NfsMount {
                    name: "media".into(),
                    server: "nas".into(),
                    path: "/media2".into(),
                    mount_path: "/media2".into(),
                    ..Default::default()
                },
            ],
        });
        let mut errors = Vec::new();
        validate_unique_volume_names(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate nfsMount name"));
    }

    // ── validate_transmission_settings ──

    #[test]
    fn transmission_settings_no_config() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_transmission_settings(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn transmission_settings_no_managed_keys() {
        let mut spec = minimal_spec(AppType::Transmission);
        spec.app_config = Some(AppConfig::Transmission(TransmissionConfig {
            settings: serde_json::json!({"speed-limit-down": 100}),
            ..Default::default()
        }));
        let mut errors = Vec::new();
        validate_transmission_settings(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn transmission_settings_with_managed_key() {
        let mut spec = minimal_spec(AppType::Transmission);
        spec.app_config = Some(AppConfig::Transmission(TransmissionConfig {
            settings: serde_json::json!({"rpc-password": "hunter2"}),
            ..Default::default()
        }));
        let mut errors = Vec::new();
        validate_transmission_settings(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("rpc-password"));
    }

    // ── validate_backup_retention ──

    #[test]
    fn backup_retention_no_backup() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_backup_retention(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn backup_retention_enabled_positive() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.backup = Some(BackupSpec {
            enabled: true,
            retention_count: 5,
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_backup_retention(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn backup_retention_enabled_zero() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.backup = Some(BackupSpec {
            enabled: true,
            retention_count: 0,
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_backup_retention(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("retentionCount"));
    }

    // ── validate_indexer_definition_names ──

    #[test]
    fn indexer_names_no_prowlarr() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_indexer_definition_names(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn indexer_names_valid() {
        let mut spec = minimal_spec(AppType::Prowlarr);
        spec.app_config = Some(AppConfig::Prowlarr(ProwlarrConfig {
            custom_definitions: vec![IndexerDefinition {
                name: "my-indexer".into(),
                content: "yaml: here".into(),
            }],
        }));
        let mut errors = Vec::new();
        validate_indexer_definition_names(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn indexer_names_empty() {
        let mut spec = minimal_spec(AppType::Prowlarr);
        spec.app_config = Some(AppConfig::Prowlarr(ProwlarrConfig {
            custom_definitions: vec![IndexerDefinition {
                name: "".into(),
                content: "yaml: here".into(),
            }],
        }));
        let mut errors = Vec::new();
        validate_indexer_definition_names(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("non-empty"));
    }

    #[test]
    fn indexer_names_special_chars() {
        let mut spec = minimal_spec(AppType::Prowlarr);
        spec.app_config = Some(AppConfig::Prowlarr(ProwlarrConfig {
            custom_definitions: vec![IndexerDefinition {
                name: "my indexer!".into(),
                content: "yaml: here".into(),
            }],
        }));
        let mut errors = Vec::new();
        validate_indexer_definition_names(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("alphanumeric"));
    }

    // ── validate_ssh_shell_override ──

    #[test]
    fn ssh_shell_override_non_ssh_app() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_ssh_shell_override(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn ssh_shell_override_interactive_mode() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig {
            users: vec![SshUser {
                name: "alice".into(),
                uid: 1000,
                gid: 1000,
                mode: SshMode::Shell,
                shell: Some("/bin/zsh".into()),
                ..Default::default()
            }],
            ..Default::default()
        }));
        let mut errors = Vec::new();
        validate_ssh_shell_override(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn ssh_shell_override_restricted_rsync() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig {
            users: vec![SshUser {
                name: "bob".into(),
                uid: 1001,
                gid: 1001,
                mode: SshMode::RestrictedRsync,
                shell: Some("/bin/bash".into()),
                ..Default::default()
            }],
            ..Default::default()
        }));
        let mut errors = Vec::new();
        validate_ssh_shell_override(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("restricted-rsync"));
    }

    // ── validate_identity_immutable ──

    fn wrap_spec_as_object(spec: &ServarrAppSpec) -> serde_json::Value {
        serde_json::json!({
            "spec": serde_json::to_value(spec).unwrap()
        })
    }

    #[test]
    fn identity_immutable_no_old_object() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_identity_immutable(&spec, None, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn identity_immutable_same_type_and_instance() {
        let old_spec = minimal_spec(AppType::Sonarr);
        let new_spec = minimal_spec(AppType::Sonarr);
        let old_obj = wrap_spec_as_object(&old_spec);
        let mut errors = Vec::new();
        validate_identity_immutable(&new_spec, Some(&old_obj), &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn identity_immutable_different_app_type() {
        let old_spec = minimal_spec(AppType::Sonarr);
        let mut new_spec = minimal_spec(AppType::Radarr);
        new_spec.instance = None;
        let old_obj = wrap_spec_as_object(&old_spec);
        let mut errors = Vec::new();
        validate_identity_immutable(&new_spec, Some(&old_obj), &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("immutable"));
        assert!(errors[0].contains("app"));
    }

    #[test]
    fn identity_immutable_different_instance() {
        let mut old_spec = minimal_spec(AppType::Sonarr);
        old_spec.instance = Some("default".into());
        let mut new_spec = minimal_spec(AppType::Sonarr);
        new_spec.instance = Some("4k".into());
        let old_obj = wrap_spec_as_object(&old_spec);
        let mut errors = Vec::new();
        validate_identity_immutable(&new_spec, Some(&old_obj), &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("immutable"));
        assert!(errors[0].contains("instance"));
    }
}
