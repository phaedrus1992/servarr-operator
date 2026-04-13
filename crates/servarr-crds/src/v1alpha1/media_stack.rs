use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::app_config::AppConfig;
use super::spec::{AppType, ServarrAppSpec, nullable_app_config_schema};
use super::status::Condition;
use super::types::*;

// ---------------------------------------------------------------------------
// MediaStack CRD
// ---------------------------------------------------------------------------

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "servarr.dev",
    version = "v1alpha1",
    kind = "MediaStack",
    namespaced,
    status = "MediaStackStatus",
    shortname = "ms",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.readyApps"}"#,
    printcolumn = r#"{"name":"Total","type":"string","jsonPath":".status.totalApps"}"#,
    printcolumn = r#"{"name":"Tier","type":"string","jsonPath":".status.currentTier","priority":1}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MediaStackSpec {
    /// In-cluster NFS server configuration. When omitted or `enabled: true`, the
    /// operator deploys an NFS server and auto-injects media mounts into every app.
    /// Set `enabled: false` to opt out entirely, or set `externalServer` to use
    /// your own NFS.
    #[serde(default)]
    pub nfs: Option<NfsServerSpec>,

    /// Shared defaults applied to every app in the stack. Per-app fields
    /// override these values.
    #[serde(default)]
    pub defaults: Option<StackDefaults>,

    /// The list of apps to deploy as part of this stack.
    pub apps: Vec<StackApp>,
}

// ---------------------------------------------------------------------------
// StackDefaults — shared config that every StackApp inherits
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StackDefaults {
    #[serde(default)]
    pub uid: Option<i64>,
    #[serde(default)]
    pub gid: Option<i64>,
    #[serde(default)]
    pub security: Option<SecurityProfile>,
    #[serde(default)]
    pub gateway: Option<GatewaySpec>,
    #[serde(default)]
    pub resources: Option<ResourceRequirements>,
    #[serde(default)]
    pub persistence: Option<PersistenceSpec>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub scheduling: Option<NodeScheduling>,
    #[serde(default)]
    pub network_policy: Option<bool>,
    #[serde(default)]
    pub network_policy_config: Option<NetworkPolicyConfig>,
    #[serde(default)]
    pub image_pull_secrets: Option<Vec<String>>,
    #[serde(default)]
    pub pod_annotations: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub admin_credentials: Option<AdminCredentialsSpec>,
}

// ---------------------------------------------------------------------------
// StackApp — per-app definition inside a MediaStack
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StackApp {
    /// The application type (required).
    pub app: AppType,

    /// Optional instance label for multi-instance deployments (e.g. "4k").
    #[serde(default)]
    pub instance: Option<String>,

    /// Whether this app is enabled. Defaults to true.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    // -- Override fields (all optional, fall back to StackDefaults) --
    #[serde(default)]
    pub image: Option<ImageSpec>,
    #[serde(default)]
    pub uid: Option<i64>,
    #[serde(default)]
    pub gid: Option<i64>,
    #[serde(default)]
    pub security: Option<SecurityProfile>,
    #[serde(default)]
    pub service: Option<ServiceSpec>,
    #[serde(default)]
    pub gateway: Option<GatewaySpec>,
    #[serde(default)]
    pub resources: Option<ResourceRequirements>,
    #[serde(default)]
    pub persistence: Option<PersistenceSpec>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub probes: Option<ProbeSpec>,
    #[serde(default)]
    pub scheduling: Option<NodeScheduling>,
    #[serde(default)]
    pub network_policy: Option<bool>,
    #[serde(default)]
    pub network_policy_config: Option<NetworkPolicyConfig>,
    #[serde(default)]
    #[schemars(schema_with = "nullable_app_config_schema")]
    pub app_config: Option<AppConfig>,
    #[serde(default)]
    pub api_key_secret: Option<String>,
    #[serde(default)]
    pub api_health_check: Option<ApiHealthCheckSpec>,
    #[serde(default)]
    pub backup: Option<BackupSpec>,
    #[serde(default)]
    pub image_pull_secrets: Option<Vec<String>>,
    #[serde(default)]
    pub pod_annotations: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub gpu: Option<GpuSpec>,
    #[serde(default)]
    pub prowlarr_sync: Option<ProwlarrSyncSpec>,
    #[serde(default)]
    pub overseerr_sync: Option<OverseerrSyncSpec>,
    #[serde(default)]
    pub admin_credentials: Option<AdminCredentialsSpec>,

    /// When true, creates both a standard and a 4K instance of this app.
    /// Only valid for Sonarr and Radarr.
    #[serde(default)]
    pub split4k: Option<bool>,

    /// Override fields applied only to the 4K instance when split4k is true.
    #[serde(default)]
    pub split4k_overrides: Option<Split4kOverrides>,
}

/// Override fields applied only to the 4K instance when `split4k` is true.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Split4kOverrides {
    #[serde(default)]
    pub image: Option<ImageSpec>,
    #[serde(default)]
    pub resources: Option<ResourceRequirements>,
    #[serde(default)]
    pub persistence: Option<PersistenceSpec>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub service: Option<ServiceSpec>,
    #[serde(default)]
    pub gateway: Option<GatewaySpec>,
    #[serde(default)]
    pub admin_credentials: Option<AdminCredentialsSpec>,
}

fn default_enabled() -> bool {
    true
}

impl StackApp {
    /// Compute the child ServarrApp name for this app inside a stack.
    ///
    /// Format: `"{stack}-{app}"` or `"{stack}-{app}-{instance}"`.
    pub fn child_name(&self, stack_name: &str) -> String {
        match &self.instance {
            Some(inst) => format!("{stack_name}-{}-{inst}", self.app.as_str()),
            None => format!("{stack_name}-{}", self.app.as_str()),
        }
    }

    /// Returns `true` if `split4k` is valid for this app type.
    /// Only Sonarr and Radarr support the split 4K pattern.
    pub fn split4k_valid(&self) -> bool {
        matches!(self.app, AppType::Sonarr | AppType::Radarr)
    }

    /// Expand this StackApp into one or two `(child_name, ServarrAppSpec)` pairs.
    ///
    /// When `split4k` is `Some(true)`, produces a base instance and a 4K instance.
    /// The 4K instance has `instance: Some("4k")` and applies any `split4k_overrides`.
    ///
    /// When `nfs` is provided, NFS media mounts are auto-injected into each spec
    /// based on app type. 4K instances receive the 4K-specific path variants.
    ///
    /// Returns `Err` if `split4k` is set on an unsupported app type.
    pub fn expand(
        &self,
        stack_name: &str,
        stack_namespace: &str,
        defaults: Option<&StackDefaults>,
        nfs: Option<&NfsServerSpec>,
    ) -> Result<Vec<(String, ServarrAppSpec)>, String> {
        if self.split4k == Some(true) && !self.split4k_valid() {
            return Err(format!(
                "split4k is only valid for Sonarr and Radarr, not {}",
                self.app.as_str()
            ));
        }

        let base_name = self.child_name(stack_name);
        let mut base_spec = self.to_servarr_spec(defaults);
        inject_nfs_mounts(&mut base_spec, nfs, false, stack_name, stack_namespace);
        let mut result = vec![(base_name, base_spec)];

        if self.split4k == Some(true) {
            // Build the 4K instance by cloning and applying overrides
            let four_k_name = format!("{stack_name}-{}-4k", self.app.as_str());

            let mut four_k_app = self.clone();
            four_k_app.instance = Some("4k".into());
            four_k_app.split4k = None; // prevent recursive expansion

            // Apply split4k_overrides
            if let Some(ref overrides) = self.split4k_overrides {
                if overrides.image.is_some() {
                    four_k_app.image = overrides.image.clone();
                }
                if overrides.resources.is_some() {
                    four_k_app.resources = overrides.resources.clone();
                }
                if overrides.persistence.is_some() {
                    four_k_app.persistence = overrides.persistence.clone();
                }
                if !overrides.env.is_empty() {
                    four_k_app.env = merge_env(&self.env, &overrides.env);
                }
                if overrides.service.is_some() {
                    four_k_app.service = overrides.service.clone();
                }
                if overrides.gateway.is_some() {
                    four_k_app.gateway = overrides.gateway.clone();
                }
                if overrides.admin_credentials.is_some() {
                    four_k_app.admin_credentials = overrides.admin_credentials.clone();
                }
            }

            let mut four_k_spec = four_k_app.to_servarr_spec(defaults);
            inject_nfs_mounts(&mut four_k_spec, nfs, true, stack_name, stack_namespace);
            result.push((four_k_name, four_k_spec));
        }

        Ok(result)
    }

    /// Merge this app's fields with stack defaults to produce a full
    /// `ServarrAppSpec`.
    pub fn to_servarr_spec(&self, defaults: Option<&StackDefaults>) -> ServarrAppSpec {
        let d = defaults.cloned().unwrap_or_default();

        // Merge env: stack defaults first, then per-app overrides by name.
        let env = merge_env(&d.env, &self.env);

        // Merge persistence: per-app PVC volumes replace stack; NFS additive
        // with dedup by name.
        let persistence = merge_persistence(d.persistence.as_ref(), self.persistence.as_ref());

        // Merge pod_annotations: stack defaults, per-app overrides matching keys.
        let pod_annotations =
            merge_annotations(d.pod_annotations.as_ref(), self.pod_annotations.as_ref());

        ServarrAppSpec {
            app: self.app.clone(),
            instance: self.instance.clone(),
            image: self.image.clone(),
            uid: self.uid.or(d.uid),
            gid: self.gid.or(d.gid),
            security: self.security.clone().or(d.security),
            service: self.service.clone(),
            gateway: self.gateway.clone().or(d.gateway),
            resources: self.resources.clone().or(d.resources),
            persistence,
            env,
            probes: self.probes.clone(),
            scheduling: self.scheduling.clone().or(d.scheduling),
            network_policy: self.network_policy.or(d.network_policy),
            network_policy_config: self
                .network_policy_config
                .clone()
                .or(d.network_policy_config),
            app_config: self.app_config.clone(),
            api_key_secret: self.api_key_secret.clone(),
            api_health_check: self.api_health_check.clone(),
            backup: self.backup.clone(),
            image_pull_secrets: self.image_pull_secrets.clone().or(d.image_pull_secrets),
            pod_annotations,
            gpu: self.gpu.clone(),
            prowlarr_sync: self.prowlarr_sync.clone(),
            overseerr_sync: self.overseerr_sync.clone(),
            admin_credentials: self.admin_credentials.clone().or(d.admin_credentials),
        }
    }
}

/// Inject auto-generated NFS mounts into a resolved app spec.
///
/// Mounts are inserted into `spec.persistence` using the additive NFS merge
/// strategy — user-provided mounts with the same name take precedence.
///
/// `is_4k` routes Sonarr and Radarr to their 4K path variants so the
/// standard and 4K instances see the same container path (`/tv`, `/movies`)
/// while pointing to separate directories on the NFS server.
fn inject_nfs_mounts(
    spec: &mut ServarrAppSpec,
    nfs: Option<&NfsServerSpec>,
    is_4k: bool,
    stack_name: &str,
    stack_namespace: &str,
) {
    let Some(nfs) = nfs else { return };
    let Some(server) = nfs.server_address(stack_name, stack_namespace) else {
        return;
    };

    // `server_path` is the NFS server-side export path; `mount_path` is the
    // container path. For 4K Sonarr/Radarr instances the server path points
    // to the 4K folder while the container still sees the standard path
    // (/tv, /movies) so app config is identical across standard and 4K instances.
    let make = |name: &str, server_path: &str, mount_path: &str| NfsMount {
        name: name.to_string(),
        server: server.clone(),
        path: nfs.nfs_path(server_path),
        mount_path: mount_path.to_string(),
        read_only: false,
    };

    let mounts: Vec<NfsMount> = match &spec.app {
        AppType::Sonarr => {
            let nfs_path = if is_4k { &nfs.tv_4k_path } else { &nfs.tv_path };
            vec![make("tv", nfs_path, &nfs.tv_path.clone())]
        }
        AppType::Radarr => {
            let nfs_path = if is_4k {
                &nfs.movies_4k_path
            } else {
                &nfs.movies_path
            };
            vec![make("movies", nfs_path, &nfs.movies_path.clone())]
        }
        AppType::Lidarr => vec![make("music", &nfs.music_path, &nfs.music_path.clone())],
        AppType::Sabnzbd | AppType::Transmission => vec![
            make("movies", &nfs.movies_path, &nfs.movies_path.clone()),
            make("tv", &nfs.tv_path, &nfs.tv_path.clone()),
            make("music", &nfs.music_path, &nfs.music_path.clone()),
            make(
                "movies-4k",
                &nfs.movies_4k_path,
                &nfs.movies_4k_path.clone(),
            ),
            make("tv-4k", &nfs.tv_4k_path, &nfs.tv_4k_path.clone()),
        ],
        AppType::Plex | AppType::Jellyfin => vec![
            make("movies", &nfs.movies_path, &nfs.movies_path.clone()),
            make("tv", &nfs.tv_path, &nfs.tv_path.clone()),
            make("music", &nfs.music_path, &nfs.music_path.clone()),
            make(
                "movies-4k",
                &nfs.movies_4k_path,
                &nfs.movies_4k_path.clone(),
            ),
            make("tv-4k", &nfs.tv_4k_path, &nfs.tv_4k_path.clone()),
        ],
        AppType::Maintainerr => vec![
            make("movies", &nfs.movies_path, &nfs.movies_path.clone()),
            make("tv", &nfs.tv_path, &nfs.tv_path.clone()),
        ],
        AppType::SshBastion => vec![
            make("movies", &nfs.movies_path, &nfs.movies_path.clone()),
            make("tv", &nfs.tv_path, &nfs.tv_path.clone()),
            make("music", &nfs.music_path, &nfs.music_path.clone()),
        ],
        _ => return,
    };

    if mounts.is_empty() {
        return;
    }

    // Merge: auto-injected mounts are the base layer; user-specified mounts
    // (from the resolved spec) take precedence by name.
    let injected = PersistenceSpec {
        volumes: Vec::new(),
        nfs_mounts: mounts,
    };
    spec.persistence = Some(match spec.persistence.take() {
        None => injected,
        Some(user) => injected.merge_with(&user),
    });
}

/// Merge env vars: stack defaults first, per-app overrides same-name entries.
fn merge_env(defaults: &[EnvVar], overrides: &[EnvVar]) -> Vec<EnvVar> {
    use indexmap::IndexMap;
    let mut map: IndexMap<String, String> = IndexMap::new();
    for e in defaults {
        map.insert(e.name.clone(), e.value.clone());
    }
    for e in overrides {
        map.insert(e.name.clone(), e.value.clone());
    }
    map.into_iter()
        .map(|(name, value)| EnvVar { name, value })
        .collect()
}

/// Merge persistence: per-app PVC volumes replace stack volumes entirely;
/// NFS mounts are additive, deduplicated by name (per-app wins).
fn merge_persistence(
    defaults: Option<&PersistenceSpec>,
    app: Option<&PersistenceSpec>,
) -> Option<PersistenceSpec> {
    match (defaults, app) {
        (None, None) => None,
        (None, Some(a)) => Some(a.clone()),
        (Some(d), None) => Some(d.clone()),
        (Some(d), Some(a)) => Some(d.merge_with(a)),
    }
}

/// Merge pod annotations: stack defaults, per-app overrides matching keys.
fn merge_annotations(
    defaults: Option<&BTreeMap<String, String>>,
    app: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    match (defaults, app) {
        (None, None) => None,
        (None, Some(a)) => Some(a.clone()),
        (Some(d), None) => Some(d.clone()),
        (Some(d), Some(a)) => {
            let mut merged = d.clone();
            merged.extend(a.iter().map(|(k, v)| (k.clone(), v.clone())));
            Some(merged)
        }
    }
}

// ---------------------------------------------------------------------------
// MediaStack Status
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaStackStatus {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub phase: StackPhase,
    #[serde(default)]
    pub current_tier: Option<u8>,
    #[serde(default)]
    pub total_apps: i32,
    #[serde(default)]
    pub ready_apps: i32,
    #[serde(default)]
    pub app_statuses: Vec<StackAppStatus>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub observed_generation: i64,
    /// RFC 3339 timestamp of when the current tier first became blocked.
    /// Reset when the tier advances. Used to enforce the tier rollout timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_blocked_since: Option<String>,
}

impl MediaStackStatus {
    pub fn set_condition(&mut self, cond: Condition) {
        if let Some(existing) = self
            .conditions
            .iter_mut()
            .find(|c| c.condition_type == cond.condition_type)
        {
            *existing = cond;
        } else {
            self.conditions.push(cond);
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum StackPhase {
    #[default]
    Pending,
    RollingOut,
    Ready,
    Degraded,
}

impl std::fmt::Display for StackPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::RollingOut => write!(f, "RollingOut"),
            Self::Ready => write!(f, "Ready"),
            Self::Degraded => write!(f, "Degraded"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StackAppStatus {
    pub name: String,
    pub app_type: String,
    pub tier: u8,
    /// True when this app's tier was advanced past without the app becoming
    /// ready (tier rollout timeout elapsed).  Cleared once the app is ready.
    #[serde(default)]
    pub bypassed: bool,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub enabled: bool,
}
