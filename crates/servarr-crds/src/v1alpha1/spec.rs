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

    /// Override the name of the generated Service (and the route backendRef
    /// that targets it). Defaults to the ServarrApp's own name. Useful for
    /// preserving in-cluster DNS names from a prior deployment so other apps'
    /// stored configs keep resolving (e.g. `transmission` instead of
    /// `media-transmission`). Does not affect the Deployment or pod labels.
    #[serde(default)]
    #[schemars(pattern("^[a-z0-9]([a-z0-9-]*[a-z0-9])?$"), length(max = 63))]
    pub service_name: Option<String>,

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

    /// Seerr cross-app synchronization. Only applies to Seerr-type apps.
    #[serde(default, alias = "overseerrSync")]
    pub seerr_sync: Option<SeerrSyncSpec>,

    /// Bazarr cross-app synchronization. Only applies to Bazarr-type apps.
    #[serde(default)]
    pub bazarr_sync: Option<BazarrSyncSpec>,

    /// Subgen cross-app synchronization. Only applies to Subgen-type apps.
    #[serde(default)]
    pub subgen_sync: Option<SubgenSyncSpec>,

    /// Maintainerr cross-app synchronization. Only applies to Maintainerr-type apps.
    #[serde(default)]
    pub maintainerr_sync: Option<MaintainerrSyncSpec>,

    /// Cleanuparr cross-app synchronization. Only applies to Cleanuparr-type apps.
    #[serde(default)]
    pub cleanuparr_sync: Option<CleanuparrSyncSpec>,

    /// Houndarr cross-app synchronization. Only applies to Houndarr-type apps.
    #[serde(default)]
    pub houndarr_sync: Option<HoundarrSyncSpec>,

    /// Admin credentials for this app. References a user-created Kubernetes Secret
    /// with `username` and `password` keys. The operator reads but never owns this secret.
    ///
    /// For Sonarr, Radarr, Lidarr, and Prowlarr: injected as `APP__AUTH__USERNAME`,
    /// `APP__AUTH__PASSWORD`, and `APP__AUTH__METHOD=Forms` env vars (requires restart).
    /// For other apps: applied via live API calls on every reconcile.
    #[serde(default)]
    pub admin_credentials: Option<AdminCredentialsSpec>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub enum AppType {
    #[default]
    Sonarr,
    Radarr,
    Lidarr,
    Prowlarr,
    Sabnzbd,
    Transmission,
    Tautulli,
    #[serde(alias = "Overseerr")]
    Seerr,
    Maintainerr,
    Jackett,
    Jellyfin,
    Plex,
    SshBastion,
    Bazarr,
    Subgen,
    Unpackerr,
    Cleanuparr,
    Houndarr,
}

/// Legacy serde aliases accepted by `AppType`'s `Deserialize` impl (e.g.
/// `#[serde(alias = "Overseerr")]` on `Seerr` above) but invisible to schemars, which has no
/// insight into serde aliases. Without merging these into the generated schema, a fresh
/// `kubectl apply` of a manifest using a pre-rename spelling is rejected outright by the CRD's
/// structural schema, even though an already-stored object with that spelling keeps
/// reconciling fine (#540). Append here whenever a variant gains a new `#[serde(alias = ...)]`.
const LEGACY_APP_TYPE_ALIASES: &[&str] = &["Overseerr"];

impl JsonSchema for AppType {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AppType".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        // Current wire values, derived from `Serialize` so this can't drift from the variant
        // list -- only the legacy-alias list above needs manual upkeep.
        let mut values: Vec<serde_json::Value> = Self::ALL
            .iter()
            .map(|v| serde_json::to_value(v).expect("AppType always serializes to a string"))
            .collect();
        values.extend(
            LEGACY_APP_TYPE_ALIASES
                .iter()
                .map(|alias| serde_json::Value::String((*alias).to_string())),
        );

        schemars::json_schema!({
            "type": "string",
            "enum": values,
        })
    }
}

impl AppType {
    /// Every app variant, in enum-declaration order. Single source of truth for
    /// "which apps exist" so callers (defaults validation, env-image-override
    /// loading) can't drift from the enum.
    pub const ALL: &'static [AppType] = &[
        Self::Sonarr,
        Self::Radarr,
        Self::Lidarr,
        Self::Prowlarr,
        Self::Sabnzbd,
        Self::Transmission,
        Self::Tautulli,
        Self::Seerr,
        Self::Maintainerr,
        Self::Jackett,
        Self::Jellyfin,
        Self::Plex,
        Self::SshBastion,
        Self::Bazarr,
        Self::Subgen,
        Self::Unpackerr,
        Self::Cleanuparr,
        Self::Houndarr,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
            Self::Lidarr => "lidarr",
            Self::Prowlarr => "prowlarr",
            Self::Sabnzbd => "sabnzbd",
            Self::Transmission => "transmission",
            Self::Tautulli => "tautulli",
            Self::Seerr => "seerr",
            Self::Maintainerr => "maintainerr",
            Self::Jackett => "jackett",
            Self::Jellyfin => "jellyfin",
            Self::Plex => "plex",
            Self::SshBastion => "ssh-bastion",
            Self::Bazarr => "bazarr",
            Self::Subgen => "subgen",
            Self::Unpackerr => "unpackerr",
            Self::Cleanuparr => "cleanuparr",
            Self::Houndarr => "houndarr",
        }
    }

    /// Return the startup tier for this app type.
    ///
    /// - Tier 0 — Infrastructure & Media Servers (Plex, Jellyfin, SshBastion)
    /// - Tier 1 — Download Clients (Sabnzbd, Transmission)
    /// - Tier 2 — Media Managers (Sonarr, Radarr, Lidarr)
    /// - Tier 3 — Ancillary (Tautulli, Seerr, Maintainerr, Prowlarr, Jackett, Bazarr, Subgen)
    pub fn tier(&self) -> u8 {
        match self {
            Self::Plex | Self::Jellyfin | Self::SshBastion => 0,
            Self::Sabnzbd | Self::Transmission => 1,
            Self::Sonarr | Self::Radarr | Self::Lidarr => 2,
            Self::Tautulli
            | Self::Seerr
            | Self::Maintainerr
            | Self::Prowlarr
            | Self::Jackett
            | Self::Bazarr
            | Self::Unpackerr
            | Self::Cleanuparr
            | Self::Houndarr
            // #10: Subgen depends on Jellyfin (subgenSync requires a Jellyfin CR) so it must
            // start after Jellyfin is ready, not at the same time (tier 0).
            | Self::Subgen => 3,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard for #540: `LEGACY_APP_TYPE_ALIASES` is a hand-maintained mirror of
    /// the `#[serde(alias = "...")]` attributes on `AppType`'s variants (schemars can't see
    /// serde aliases, so this list exists to fold them into the generated schema). A typo'd
    /// or stale entry would make the CRD schema *accept* a value the operator's Deserialize
    /// then rejects -- the exact failure mode #540 was about, inverted.
    #[test]
    fn every_legacy_app_type_alias_deserializes() {
        for alias in LEGACY_APP_TYPE_ALIASES {
            let parsed: Result<AppType, _> = serde_json::from_value(serde_json::json!(alias));
            assert!(
                parsed.is_ok(),
                "LEGACY_APP_TYPE_ALIASES entry {alias:?} does not deserialize to a valid \
                 AppType -- the schema would accept a value the operator rejects"
            );
        }
    }

    /// Guards against the schema silently growing extra or duplicate entries: it must
    /// contain exactly one value per current variant plus one per legacy alias, no more.
    #[test]
    fn schema_enum_length_matches_all_variants_plus_legacy_aliases() {
        let schema = schemars::schema_for!(AppType);
        let enum_len = schema
            .get("enum")
            .and_then(|v| v.as_array())
            .expect("AppType schema should have an enum array")
            .len();
        assert_eq!(
            enum_len,
            AppType::ALL.len() + LEGACY_APP_TYPE_ALIASES.len(),
            "schema enum length drifted from AppType::ALL + LEGACY_APP_TYPE_ALIASES"
        );
    }
}
