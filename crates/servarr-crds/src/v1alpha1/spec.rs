use kube::CustomResource;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use super::app_config::AppConfig;
use super::status::ServarrAppStatus;
use super::types::*;

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "servarr.dev",
    version = "v1alpha1",
    kind = "ServarrApp",
    namespaced,
    status = "ServarrAppStatus",
    shortname = "sa",
    printcolumn = r#"{"name":"App","type":"string","jsonPath":".spec.app"}"#,
    printcolumn = r#"{"name":"Instance","type":"string","jsonPath":".spec.instance","priority":1}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ServarrAppSpec {
    pub app: AppType,

    /// Optional instance label (e.g. "4k", "anime") to distinguish multiple
    /// instances of the same app type within a namespace.
    #[serde(default)]
    pub instance: Option<String>,

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

    /// Fine-grained NetworkPolicy configuration. Takes precedence over the
    /// boolean `network_policy` flag when set.
    #[serde(default)]
    pub network_policy_config: Option<NetworkPolicyConfig>,

    #[serde(default)]
    #[schemars(schema_with = "nullable_app_config_schema")]
    pub app_config: Option<AppConfig>,

    /// Name of a Kubernetes Secret containing an `api-key` data field.
    /// Used for API health checks and backup operations.
    #[serde(default)]
    pub api_key_secret: Option<String>,

    /// API-driven health check configuration.
    #[serde(default)]
    pub api_health_check: Option<ApiHealthCheckSpec>,

    /// Backup configuration via the app's API.
    #[serde(default)]
    pub backup: Option<BackupSpec>,

    /// Names of Kubernetes Secrets for private registry authentication.
    #[serde(default)]
    pub image_pull_secrets: Option<Vec<String>>,

    /// Additional annotations to add to the pod template.
    #[serde(default)]
    pub pod_annotations: Option<std::collections::BTreeMap<String, String>>,

    /// GPU passthrough configuration for hardware-accelerated transcoding.
    #[serde(default)]
    pub gpu: Option<GpuSpec>,

    /// Prowlarr cross-app synchronization. Only applies to Prowlarr-type apps.
    #[serde(default)]
    pub prowlarr_sync: Option<ProwlarrSyncSpec>,

    /// Overseerr cross-app synchronization. Only applies to Overseerr-type apps.
    #[serde(default)]
    pub overseerr_sync: Option<OverseerrSyncSpec>,

    /// Bazarr cross-app synchronization. Only applies to Bazarr-type apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bazarr_sync: Option<BazarrSyncSpec>,

    /// Subgen cross-app synchronization. Only applies to Subgen-type apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgen_sync: Option<SubgenSyncSpec>,

    /// Admin credentials for this app. References a user-created Kubernetes Secret
    /// with `username` and `password` keys. The operator reads but never owns this secret.
    ///
    /// For Sonarr, Radarr, Lidarr, and Prowlarr: injected as `APP__AUTH__USERNAME`,
    /// `APP__AUTH__PASSWORD`, and `APP__AUTH__METHOD=Forms` env vars (requires restart).
    /// For other apps: applied via live API calls on every reconcile.
    #[serde(default)]
    pub admin_credentials: Option<AdminCredentialsSpec>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum AppType {
    #[default]
    Sonarr,
    Radarr,
    Lidarr,
    Prowlarr,
    Sabnzbd,
    Transmission,
    Tautulli,
    Overseerr,
    Maintainerr,
    Jackett,
    Jellyfin,
    Plex,
    SshBastion,
    Bazarr,
    Subgen,
}

impl AppType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
            Self::Lidarr => "lidarr",
            Self::Prowlarr => "prowlarr",
            Self::Sabnzbd => "sabnzbd",
            Self::Transmission => "transmission",
            Self::Tautulli => "tautulli",
            Self::Overseerr => "overseerr",
            Self::Maintainerr => "maintainerr",
            Self::Jackett => "jackett",
            Self::Jellyfin => "jellyfin",
            Self::Plex => "plex",
            Self::SshBastion => "ssh-bastion",
            Self::Bazarr => "bazarr",
            Self::Subgen => "subgen",
        }
    }

    /// Return the startup tier for this app type.
    ///
    /// - Tier 0 — Infrastructure & Media Servers (Plex, Jellyfin, SshBastion)
    /// - Tier 1 — Download Clients (Sabnzbd, Transmission)
    /// - Tier 2 — Media Managers (Sonarr, Radarr, Lidarr)
    /// - Tier 3 — Ancillary (Tautulli, Overseerr, Maintainerr, Prowlarr, Jackett)
    pub fn tier(&self) -> u8 {
        match self {
            Self::Plex | Self::Jellyfin | Self::SshBastion | Self::Subgen => 0,
            Self::Sabnzbd | Self::Transmission => 1,
            Self::Sonarr | Self::Radarr | Self::Lidarr => 2,
            Self::Tautulli
            | Self::Overseerr
            | Self::Maintainerr
            | Self::Prowlarr
            | Self::Jackett
            | Self::Bazarr => 3,
        }
    }

    pub fn tier_name(tier: u8) -> &'static str {
        match tier {
            0 => "MediaServers",
            1 => "DownloadClients",
            2 => "MediaManagers",
            3 => "Ancillary",
            _ => "Unknown",
        }
    }
}

impl std::fmt::Display for AppType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Produce a K8s-structural-schema-compatible nullable schema for `AppConfig`.
///
/// The default `Option<AppConfig>` schema uses `anyOf[{oneOf: [...]}, {nullable: true}]`
/// which Kubernetes rejects. Instead we generate the `AppConfig` schema directly
/// and set `nullable: true` at the top level.
pub(crate) fn nullable_app_config_schema(generator: &mut SchemaGenerator) -> Schema {
    let mut schema = generator.subschema_for::<AppConfig>();
    schema.insert("nullable".to_string(), serde_json::Value::Bool(true));
    schema
}
