use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
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
use servarr_api::k8s::{kube_err_public_summary, kube_err_summary};
use servarr_crds::{
    AppConfig, AppDefaults, AppType, RouteType, ServarrApp, ServarrAppSpec, SshMode,
};
use tracing::{debug, info, warn};

use crate::controller::normalize_backup_schedule;

const DEFAULT_WEBHOOK_PORT: u16 = 9443;

const DEFAULT_TLS_DIR: &str = "/etc/webhook/tls";

/// Configuration for the webhook server.
#[derive(Clone)]
pub struct WebhookConfig {
    pub port: u16,
    tls_cert: PathBuf,
    tls_key: PathBuf,
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

    // Rule 5: persistence mount-path / volume-name collisions (including reserved operator
    // names/paths, same-list duplicate names, and non-canonical '..' mountPaths) — the same
    // checks AppDefaults::resolve_persistence runs at reconcile time (#486)
    validate_persistence_collisions(&parsed, &mut errors);

    // Rule 5b: removedDefaultVolumes must name an actual default volume
    validate_removed_default_volumes(&parsed, &mut errors);

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

    // Rule 12: backup.schedule must be a valid cron expression
    validate_backup_schedule(&parsed, &mut errors);

    // Rule 13: SshBastion security context must not break init container
    validate_ssh_security_context(&parsed, &mut errors);

    // Rule 14: SSH bastion user names and allowed paths must be safe for shell interpolation
    validate_ssh_bastion_inputs(&parsed, &mut errors);

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
    let old_spec_value = old_object.and_then(|o| o.get("spec"));
    let old_spec =
        old_spec_value.and_then(|s| serde_json::from_value::<ServarrAppSpec>(s.clone()).ok());

    if old_spec_value.is_some() && old_spec.is_none() {
        // #720: the stored object's spec failed to parse -- silently treating that the same as
        // "no old object" would skip the identity-immutability check entirely instead of
        // rejecting the ambiguous state. Reject rather than fail open (mirrors the pattern in
        // `validate_persistence_collisions`/`validate_removed_default_volumes`, #716).
        errors.push(
            "internal error: stored object's spec could not be parsed -- identity \
             immutability (spec.app/spec.instance) could not be validated"
                .to_string(),
        );
        return;
    }

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

fn validate_backup_schedule(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref backup) = spec.backup
        && !backup.schedule.trim().is_empty()
    {
        let normalized = normalize_backup_schedule(&backup.schedule);
        match cron::Schedule::from_str(&normalized) {
            Ok(_) => {}
            Err(e) => {
                errors.push(format!(
                    "backup.schedule '{}' is not a valid cron expression: {}",
                    backup.schedule.trim(),
                    e
                ));
            }
        }
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
                | (AppType::Seerr, AppConfig::Seerr(_))
                | (AppType::Lidarr, AppConfig::Lidarr(_))
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
    let Some(ref gw) = spec.gateway else { return };
    if !gw.is_enabled() {
        return;
    }
    match gw.effective_route_type(&spec.app) {
        RouteType::Http => {
            if gw.hosts.is_empty() {
                errors
                    .push("gateway.hosts must be non-empty when an HTTP gateway is enabled".into());
            }
        }
        RouteType::Tcp => {
            if !gw.hosts.is_empty() {
                errors.push(
                    "gateway.hosts is not supported for TCP routes (TCPRoute has no hostname \
                     field); remove hosts or switch to an HTTP route type"
                        .into(),
                );
            }
        }
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
            warn!(error = %kube_err_summary(&e), "failed to list ServarrApps for duplicate check");
            errors.push(format!(
                "failed to check for duplicate instances: {}",
                kube_err_public_summary(&e)
            ));
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

/// Unwraps a `try_for_app` result for a validator, rejecting the CR instead of silently
/// admitting it if the lookup ever fails (#716). Takes the already-computed `Result` (rather
/// than calling `try_for_app` itself) so the failure path is unit-testable directly — `AppType`
/// is a closed enum with an `image-defaults.toml` entry for every variant
/// (`AppDefaults::validate_all_passes_for_every_app_type`), so this branch can't be driven from
/// a real spec, only from a synthetic `Err` in a test.
fn require_defaults(
    result: Result<AppDefaults, String>,
    app: &AppType,
    context: &str,
    errors: &mut Vec<String>,
) -> Option<AppDefaults> {
    match result {
        Ok(defaults) => Some(defaults),
        Err(_) => {
            errors.push(format!(
                "internal error: no compiled defaults for app type '{app}' -- {context} could \
                 not be validated; this indicates a broken image-defaults.toml entry"
            ));
            None
        }
    }
}

/// Runs the same merge + collision checks `AppDefaults::resolve_persistence` runs at
/// reconcile time — same-list duplicate volume/nfsMount names, mount-path collisions,
/// volume-name collisions (including a name matching one the operator reserves for itself),
/// and a non-canonical `..` mountPath segment — so a colliding spec is rejected at
/// `kubectl apply` time instead of only surfacing later as a reconcile-time `ReconcileError`
/// (#486).
fn validate_persistence_collisions(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    let Some(defaults) = require_defaults(
        AppDefaults::try_for_app(&spec.app),
        &spec.app,
        "persistence",
        errors,
    ) else {
        return;
    };
    if let Err(e) = defaults.resolve_persistence_for_spec(spec) {
        errors.push(e);
    }
}

/// `removedDefaultVolumes` names a compiled default volume to drop. A typo
/// silently no-ops (the tombstone matches nothing), so reject any entry that
/// doesn't match one of this app type's actual default volume names.
fn validate_removed_default_volumes(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    let Some(ref persistence) = spec.persistence else {
        return;
    };
    if persistence.removed_default_volumes.is_empty() {
        return;
    }
    let Some(defaults) = require_defaults(
        AppDefaults::try_for_app(&spec.app),
        &spec.app,
        "removedDefaultVolumes",
        errors,
    ) else {
        return;
    };
    for name in &persistence.removed_default_volumes {
        if !defaults.persistence.volumes.iter().any(|v| &v.name == name) {
            errors.push(format!(
                "persistence.removedDefaultVolumes references '{name}', which is not a default \
                 volume for app type '{}'",
                spec.app
            ));
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

fn validate_ssh_security_context(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    let Some(AppConfig::SshBastion(_)) = spec.app_config else {
        return;
    };

    let security = spec.security.as_ref();
    let persistence = spec.persistence.as_ref();

    if let Some(sec) = security {
        if sec.read_only_root_filesystem == Some(true) {
            let has_writable_auth_keys = persistence
                .map(|p| p.volumes.iter().any(|v| v.name == "authorized-keys"))
                .unwrap_or(false);

            if !has_writable_auth_keys {
                errors.push(
                    "SshBastion with readOnlyRootFilesystem: true must have a writable \
                     'authorized-keys' volume for the copy-authorized-keys init container"
                        .to_string(),
                );
            }
        }

        if sec.run_as_non_root == Some(true) {
            let has_chown_capability = sec
                .capabilities_add
                .iter()
                .any(|cap| cap.to_uppercase() == "CHOWN");

            if !has_chown_capability {
                errors.push(
                    "SshBastion with runAsNonRoot: true must include CHOWN capability for the \
                     copy-authorized-keys init container to set authorized_keys ownership"
                        .to_string(),
                );
            }
        }
    }
}

fn validate_ssh_bastion_inputs(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    let Some(AppConfig::SshBastion(ref sc)) = spec.app_config else {
        return;
    };
    for user in &sc.users {
        if !is_valid_ssh_username(&user.name) {
            errors.push(format!(
                "appConfig.sshBastion.users[].name {:?} must match ^[a-z_][a-z0-9_-]{{0,31}}$",
                user.name
            ));
        }
        if let Some(ref rr) = user.restricted_rsync {
            for path in &rr.allowed_paths {
                if !is_valid_allowed_path(path) {
                    errors.push(format!(
                        "appConfig.sshBastion.users[].restrictedRsync.allowedPaths {:?} must be \
                         an absolute path containing no shell metacharacters",
                        path
                    ));
                }
            }
        }
        if let Some(ref shell) = user.shell
            && !is_valid_shell_path(shell)
        {
            errors.push(format!(
                "appConfig.sshBastion.users[].shell {:?} must be an absolute path \
                 containing no colons or shell metacharacters",
                shell
            ));
        }
    }
}

fn is_valid_ssh_username(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !matches!(first, 'a'..='z' | '_') {
        return false;
    }
    // All valid chars are ASCII, so byte length == char count.
    name.len() <= 32 && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

fn is_valid_allowed_path(path: &str) -> bool {
    path.starts_with('/')
        && !path
            .chars()
            .any(|c| matches!(c, '"' | '\\' | '$' | '`') || c.is_whitespace())
}

fn is_valid_shell_path(shell: &str) -> bool {
    // Colon is also forbidden: shell is embedded in a colon-delimited passwd-format record.
    shell.starts_with('/')
        && !shell
            .chars()
            .any(|c| matches!(c, ':' | '"' | '\\' | '$' | '`') || c.is_whitespace())
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
    use serde_json::json;
    use servarr_crds::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::testutils::build_mock_client;

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
    fn app_config_match_seerr_ok() {
        let mut spec = minimal_spec(AppType::Seerr);
        spec.app_config = Some(AppConfig::Seerr(Box::default()));
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
    fn app_config_match_lidarr_ok() {
        let mut spec = minimal_spec(AppType::Lidarr);
        spec.app_config = Some(AppConfig::Lidarr(LidarrConfig::default()));
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
            enabled: Some(false),
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
            enabled: Some(true),
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
            enabled: Some(true),
            hosts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_gateway_hosts(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("non-empty"));
    }

    #[test]
    fn gateway_hosts_tcp_route_empty_hosts_ok() {
        // SshBastion is always TCP; empty hosts must pass (TCPRoute has no hostname field).
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.gateway = Some(GatewaySpec {
            enabled: Some(true),
            hosts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_gateway_hosts(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn gateway_hosts_tcp_route_non_empty_hosts_rejected() {
        // Non-empty hosts on a TCP route are silently discarded by the Gateway API —
        // surface that as a validation error rather than silently accepting bad config.
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.gateway = Some(GatewaySpec {
            enabled: Some(true),
            hosts: vec!["bastion.example.com".into()],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_gateway_hosts(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("TCP"));
    }

    // ── require_defaults (#716) ──

    #[test]
    fn require_defaults_ok_returns_defaults_no_error() {
        let mut errors = Vec::new();
        let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
        let result = require_defaults(Ok(defaults), &AppType::Sonarr, "persistence", &mut errors);
        assert!(result.is_some());
        assert!(errors.is_empty());
    }

    #[test]
    fn require_defaults_err_pushes_error_and_returns_none() {
        let mut errors = Vec::new();
        let result = require_defaults(
            Err("no image defaults for app: bogus".to_string()),
            &AppType::Sonarr,
            "persistence",
            &mut errors,
        );
        assert!(result.is_none());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("persistence"), "got: {}", errors[0]);
        assert!(
            errors[0].contains(&AppType::Sonarr.to_string()),
            "got: {}",
            errors[0]
        );
    }

    // ── validate_persistence_collisions ──

    #[test]
    fn persistence_collisions_unique_names_and_paths_ok() {
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
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_persistence_collisions(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn persistence_collisions_duplicate_volume_name_rejected() {
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
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_persistence_collisions(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate volume name: 'config'"));
    }

    #[test]
    fn persistence_collisions_duplicate_nfs_name_rejected() {
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
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_persistence_collisions(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate nfsMount name"));
    }

    /// A volume *name* matching one the operator reserves for itself must be rejected at
    /// admission time, not only surfaced later as a reconcile-time error (#485, #486).
    #[test]
    fn persistence_collisions_reserved_volume_name_rejected() {
        let mut spec = minimal_spec(AppType::Transmission);
        spec.admin_credentials = Some(AdminCredentialsSpec {
            secret_name: "transmission-admin".into(),
        });
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "admin-credentials".into(),
                mount_path: "/unrelated".into(),
                ..Default::default()
            }],
            nfs_mounts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_persistence_collisions(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("reserved by the operator"));
    }

    /// A mountPath colliding with an operator-reserved mount must be rejected at admission
    /// time — previously this only surfaced at reconcile time (#486).
    #[test]
    fn persistence_collisions_reserved_mount_path_rejected() {
        let mut spec = minimal_spec(AppType::Transmission);
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "override".into(),
                mount_path: "/watch".into(),
                ..Default::default()
            }],
            nfs_mounts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_persistence_collisions(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("reserved by the operator"));
    }

    /// A `..` segment in a mountPath must be rejected at admission time, matching the
    /// reconcile-time behavior added by #487.
    #[test]
    fn persistence_collisions_parent_segment_rejected() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "scratch".into(),
                mount_path: "/config/../scratch".into(),
                ..Default::default()
            }],
            nfs_mounts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_persistence_collisions(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("must not contain '..'"));
    }

    // ── validate_removed_default_volumes ──

    #[test]
    fn removed_default_volumes_valid_name_ok() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.persistence = Some(PersistenceSpec {
            removed_default_volumes: vec!["downloads".into()],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_removed_default_volumes(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn removed_default_volumes_typo_rejected() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.persistence = Some(PersistenceSpec {
            removed_default_volumes: vec!["download".into()],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_removed_default_volumes(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("download"));
    }

    #[test]
    fn removed_default_volumes_empty_ok() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_removed_default_volumes(&spec, &mut errors);
        assert!(errors.is_empty());
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

    // ── validate_ssh_security_context ──

    #[test]
    fn ssh_security_context_non_ssh_app() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn ssh_security_context_readonly_without_auth_keys_volume() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.security = Some(SecurityProfile {
            profile_type: SecurityProfileType::Custom,
            user: 1000,
            group: 1000,
            run_as_non_root: Some(false),
            read_only_root_filesystem: Some(true),
            allow_privilege_escalation: Some(false),
            capabilities_add: vec![],
            capabilities_drop: vec![],
        });
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("readOnlyRootFilesystem"));
        assert!(errors[0].contains("authorized-keys"));
    }

    #[test]
    fn ssh_security_context_readonly_with_auth_keys_volume() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.security = Some(SecurityProfile {
            profile_type: SecurityProfileType::Custom,
            user: 1000,
            group: 1000,
            run_as_non_root: Some(false),
            read_only_root_filesystem: Some(true),
            allow_privilege_escalation: Some(false),
            capabilities_add: vec![],
            capabilities_drop: vec![],
        });
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "authorized-keys".into(),
                mount_path: "/etc/authorized_keys".into(),
                ..Default::default()
            }],
            nfs_mounts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn ssh_security_context_nonroot_without_chown_capability() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.security = Some(SecurityProfile {
            profile_type: SecurityProfileType::Custom,
            user: 1000,
            group: 1000,
            run_as_non_root: Some(true),
            read_only_root_filesystem: Some(false),
            allow_privilege_escalation: Some(false),
            capabilities_add: vec![],
            capabilities_drop: vec![],
        });
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("runAsNonRoot"));
        assert!(errors[0].contains("CHOWN"));
    }

    #[test]
    fn ssh_security_context_nonroot_with_chown_capability() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.security = Some(SecurityProfile {
            profile_type: SecurityProfileType::Custom,
            user: 1000,
            group: 1000,
            run_as_non_root: Some(true),
            read_only_root_filesystem: Some(false),
            allow_privilege_escalation: Some(false),
            capabilities_add: vec!["CHOWN".into()],
            capabilities_drop: vec![],
        });
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn ssh_security_context_combined_constraints() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.security = Some(SecurityProfile {
            profile_type: SecurityProfileType::Custom,
            user: 1000,
            group: 1000,
            run_as_non_root: Some(true),
            read_only_root_filesystem: Some(true),
            allow_privilege_escalation: Some(false),
            capabilities_add: vec!["CHOWN".into()],
            capabilities_drop: vec![],
        });
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "authorized-keys".into(),
                mount_path: "/etc/authorized_keys".into(),
                ..Default::default()
            }],
            nfs_mounts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn ssh_security_context_no_security_field() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.security = None;
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn ssh_security_context_both_flags_missing_auth_keys() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.security = Some(SecurityProfile {
            profile_type: SecurityProfileType::Custom,
            user: 1000,
            group: 1000,
            run_as_non_root: Some(true),
            read_only_root_filesystem: Some(true),
            allow_privilege_escalation: Some(false),
            capabilities_add: vec!["CHOWN".into()],
            capabilities_drop: vec![],
        });
        spec.persistence = None;
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("readOnlyRootFilesystem"));
        assert!(errors[0].contains("authorized-keys"));
    }

    #[test]
    fn ssh_security_context_both_flags_missing_chown() {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig::default()));
        spec.security = Some(SecurityProfile {
            profile_type: SecurityProfileType::Custom,
            user: 1000,
            group: 1000,
            run_as_non_root: Some(true),
            read_only_root_filesystem: Some(true),
            allow_privilege_escalation: Some(false),
            capabilities_add: vec![],
            capabilities_drop: vec![],
        });
        spec.persistence = Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "authorized-keys".into(),
                mount_path: "/etc/authorized_keys".into(),
                ..Default::default()
            }],
            nfs_mounts: vec![],
            ..Default::default()
        });
        let mut errors = Vec::new();
        validate_ssh_security_context(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("runAsNonRoot"));
        assert!(errors[0].contains("CHOWN"));
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

    #[test]
    fn identity_immutable_malformed_old_spec_rejects() {
        // #720: `old_object.spec` present but failing to deserialize as `ServarrAppSpec` (e.g. a
        // stored object from an incompatible version) must not silently skip the immutability
        // check -- that's a fail-open on the one layer whose job is to reject it.
        let new_spec = minimal_spec(AppType::Sonarr);
        let old_obj = serde_json::json!({ "spec": {} }); // missing required `app` field
        let mut errors = Vec::new();
        validate_identity_immutable(&new_spec, Some(&old_obj), &mut errors);
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("identity"), "got {errors:?}");
    }

    // ── validate_backup_schedule ──

    #[test]
    fn backup_schedule_no_backup_config() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_backup_schedule(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn backup_schedule_empty_string() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.backup = Some(BackupSpec {
            enabled: true,
            schedule: "".into(),
            retention_count: 5,
        });
        let mut errors = Vec::new();
        validate_backup_schedule(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn backup_schedule_whitespace_only() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.backup = Some(BackupSpec {
            enabled: true,
            schedule: "   ".into(),
            retention_count: 5,
        });
        let mut errors = Vec::new();
        validate_backup_schedule(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn backup_schedule_valid_six_field_cron() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.backup = Some(BackupSpec {
            enabled: true,
            schedule: "0 3 * * * *".into(),
            retention_count: 5,
        });
        let mut errors = Vec::new();
        validate_backup_schedule(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn backup_schedule_valid_five_field_cron() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.backup = Some(BackupSpec {
            enabled: true,
            schedule: "0 3 * * *".into(),
            retention_count: 5,
        });
        let mut errors = Vec::new();
        validate_backup_schedule(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn backup_schedule_invalid_cron() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.backup = Some(BackupSpec {
            enabled: true,
            schedule: "not a cron expression".into(),
            retention_count: 5,
        });
        let mut errors = Vec::new();
        validate_backup_schedule(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("valid cron"));
        assert!(errors[0].contains("not a cron expression"));
    }

    #[test]
    fn backup_schedule_invalid_numeric_cron() {
        let mut spec = minimal_spec(AppType::Sonarr);
        spec.backup = Some(BackupSpec {
            enabled: true,
            schedule: "99 99 99 99 99".into(),
            retention_count: 5,
        });
        let mut errors = Vec::new();
        validate_backup_schedule(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("valid cron"));
    }

    // ── validate_ssh_bastion_inputs ──

    fn ssh_bastion_spec_with_user(name: &str, allowed_paths: Vec<String>) -> ServarrAppSpec {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig {
            users: vec![SshUser {
                name: name.into(),
                uid: 1000,
                gid: 1000,
                mode: SshMode::RestrictedRsync,
                restricted_rsync: Some(RestrictedRsyncConfig { allowed_paths }),
                ..Default::default()
            }],
            ..Default::default()
        }));
        spec
    }

    #[test]
    fn ssh_bastion_inputs_valid_name_and_path() {
        let spec = ssh_bastion_spec_with_user("alice_backup", vec!["/media/shows".into()]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn ssh_bastion_inputs_non_ssh_app() {
        let spec = minimal_spec(AppType::Sonarr);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn ssh_bastion_inputs_empty_username() {
        let spec = ssh_bastion_spec_with_user("", vec![]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("name"));
    }

    #[test]
    fn ssh_bastion_inputs_username_injection() {
        let spec = ssh_bastion_spec_with_user("foo; reboot", vec![]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("name"));
    }

    #[test]
    fn ssh_bastion_inputs_username_uppercase() {
        let spec = ssh_bastion_spec_with_user("Alice", vec![]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn ssh_bastion_inputs_username_starts_with_digit() {
        let spec = ssh_bastion_spec_with_user("1user", vec![]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn ssh_bastion_inputs_username_too_long() {
        let name = "a".repeat(33);
        let spec = ssh_bastion_spec_with_user(&name, vec![]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn ssh_bastion_inputs_path_relative() {
        let spec = ssh_bastion_spec_with_user("alice", vec!["media/shows".into()]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("allowedPaths"));
    }

    #[test]
    fn ssh_bastion_inputs_path_quote_injection() {
        let spec = ssh_bastion_spec_with_user("alice", vec!["/media\" /etc \"".into()]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn ssh_bastion_inputs_path_dollar_injection() {
        let spec = ssh_bastion_spec_with_user("alice", vec!["/media/$HOME".into()]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn ssh_bastion_inputs_path_backtick_injection() {
        let spec = ssh_bastion_spec_with_user("alice", vec!["/media/`id`".into()]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn ssh_bastion_inputs_path_whitespace() {
        let spec = ssh_bastion_spec_with_user("alice", vec!["/media/my shows".into()]);
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
    }

    // ── user.shell validation ──

    fn spec_with_shell(shell: Option<&str>) -> ServarrAppSpec {
        let mut spec = minimal_spec(AppType::SshBastion);
        spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig {
            users: vec![SshUser {
                name: "alice".into(),
                uid: 1000,
                gid: 1000,
                mode: SshMode::Shell,
                shell: shell.map(str::to_owned),
                ..Default::default()
            }],
            ..Default::default()
        }));
        spec
    }

    #[test]
    fn ssh_bastion_inputs_shell_valid() {
        let spec = spec_with_shell(Some("/bin/bash"));
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn ssh_bastion_inputs_shell_relative() {
        let spec = spec_with_shell(Some("bash"));
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("shell"));
    }

    #[test]
    fn ssh_bastion_inputs_shell_with_colon() {
        let spec = spec_with_shell(Some("/bin/bash:/etc/passwd"));
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("shell"));
    }

    #[test]
    fn ssh_bastion_inputs_shell_with_newline() {
        let spec = spec_with_shell(Some("/bin/bash\n/bin/sh"));
        let mut errors = Vec::new();
        validate_ssh_bastion_inputs(&spec, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("shell"));
    }

    // ── validate_no_duplicate_instance (admission-rejection message sanitization) ──

    #[tokio::test]
    async fn validate_spec_duplicate_check_error_sanitizes_message() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        // The list call fails with a 403 whose message echoes a service-account name — must not
        // leak into the admission-webhook rejection message shown to whoever ran `kubectl apply`.
        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {},
                "status": "Failure",
                "message": "forbidden: User \"system:serviceaccount:test:leaked-sa\" cannot list",
                "reason": "Forbidden",
                "code": 403
            })))
            .mount(&mock_server)
            .await;

        let object = json!({ "spec": { "app": "Sonarr" } });

        let result = validate_spec(&object, None, "CREATE", "test", &client).await;

        let err =
            result.expect_err("expected the duplicate-check API failure to surface as an error");
        assert!(err.contains("403"), "should keep the status code: {err}");
        assert!(
            !err.contains("leaked-sa"),
            "must not leak the raw API server message: {err}"
        );
    }
}
