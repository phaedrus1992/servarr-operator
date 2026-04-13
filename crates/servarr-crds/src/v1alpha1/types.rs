use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageSpec {
    pub repository: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default = "default_pull_policy")]
    pub pull_policy: String,
}

fn default_pull_policy() -> String {
    "IfNotPresent".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PvcVolume {
    pub name: String,
    pub mount_path: String,
    #[serde(default = "default_access_mode")]
    pub access_mode: String,
    #[serde(default = "default_pvc_size")]
    pub size: String,
    #[serde(default)]
    pub storage_class: String,
}

fn default_access_mode() -> String {
    "ReadWriteOnce".to_string()
}

fn default_pvc_size() -> String {
    "1Gi".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NfsMount {
    pub name: String,
    pub server: String,
    pub path: String,
    pub mount_path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersistenceSpec {
    #[serde(default)]
    pub volumes: Vec<PvcVolume>,
    #[serde(default)]
    pub nfs_mounts: Vec<NfsMount>,
}

impl PersistenceSpec {
    /// Merge `self` (base layer) with `over` (higher-priority layer).
    ///
    /// - PVC volumes: `over.volumes` replaces entirely when non-empty; base
    ///   volumes are used when `over.volumes` is empty.
    /// - NFS mounts: additive, deduplicated by name (`over` wins on conflict).
    pub fn merge_with(&self, over: &PersistenceSpec) -> PersistenceSpec {
        let volumes = if over.volumes.is_empty() {
            self.volumes.clone()
        } else {
            over.volumes.clone()
        };

        let mut nfs_map: IndexMap<String, NfsMount> = IndexMap::new();
        for m in &self.nfs_mounts {
            nfs_map.insert(m.name.clone(), m.clone());
        }
        for m in &over.nfs_mounts {
            nfs_map.insert(m.name.clone(), m.clone());
        }

        PersistenceSpec {
            volumes,
            nfs_mounts: nfs_map.into_values().collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySpec {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_route_type")]
    pub route_type: RouteType,
    #[serde(default)]
    pub parent_refs: Vec<GatewayParentRef>,
    #[serde(default)]
    pub hosts: Vec<String>,
    /// TLS configuration. When enabled, the controller creates a cert-manager
    /// Certificate and uses a TCPRoute instead of an HTTPRoute.
    #[serde(default)]
    pub tls: Option<TlsSpec>,
}

/// TLS termination via cert-manager.
///
/// When `enabled` is true the operator creates a cert-manager `Certificate`
/// resource referencing the given `cert_issuer` and switches the route type
/// from HTTPRoute to TCPRoute for TLS pass-through.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TlsSpec {
    /// Whether TLS is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Name of the cert-manager ClusterIssuer or Issuer to use.
    #[serde(default)]
    pub cert_issuer: String,
    /// Override for the TLS Secret name. If omitted, derived from the app name.
    #[serde(default)]
    pub secret_name: Option<String>,
}

fn default_route_type() -> RouteType {
    RouteType::Http
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub enum RouteType {
    #[default]
    Http,
    Tcp,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GatewayParentRef {
    pub name: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub section_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    #[serde(default = "default_service_type")]
    pub service_type: String,
    pub ports: Vec<ServicePort>,
}

fn default_service_type() -> String {
    "ClusterIP".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServicePort {
    pub name: String,
    pub port: i32,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub container_port: Option<i32>,
    #[serde(default)]
    pub host_port: Option<i32>,
}

fn default_protocol() -> String {
    "TCP".to_string()
}

/// Security profile for the container.
///
/// `profileType` selects the security model:
/// - `LinuxServer` (default): s6-overlay images needing CHOWN/SETGID/SETUID.
///   Uses `user`/`group` for PUID/PGID env vars and fsGroup.
/// - `NonRoot`: Images that run as a non-root user natively.
///   Uses `user`/`group` for runAsUser/runAsGroup/fsGroup.
/// - `Custom`: Full control over security context fields.
///   Uses all fields including capabilities, readOnlyRootFilesystem, etc.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecurityProfile {
    #[serde(default)]
    pub profile_type: SecurityProfileType,
    #[serde(default = "default_uid")]
    pub user: i64,
    #[serde(default = "default_uid")]
    pub group: i64,
    /// Override runAsNonRoot. Derived from profile_type if not set.
    #[serde(default)]
    pub run_as_non_root: Option<bool>,
    /// Override readOnlyRootFilesystem (default: false).
    #[serde(default)]
    pub read_only_root_filesystem: Option<bool>,
    /// Override allowPrivilegeEscalation (default: false).
    #[serde(default)]
    pub allow_privilege_escalation: Option<bool>,
    /// Additional Linux capabilities to add.
    #[serde(default)]
    pub capabilities_add: Vec<String>,
    /// Linux capabilities to drop (default: ["ALL"] for LinuxServer/NonRoot).
    #[serde(default)]
    pub capabilities_drop: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub enum SecurityProfileType {
    #[default]
    LinuxServer,
    NonRoot,
    Custom,
}

fn default_uid() -> i64 {
    65534
}

impl SecurityProfile {
    pub fn linux_server(user: i64, group: i64) -> Self {
        Self {
            profile_type: SecurityProfileType::LinuxServer,
            user,
            group,
            ..Default::default()
        }
    }

    pub fn non_root(user: i64, group: i64) -> Self {
        Self {
            profile_type: SecurityProfileType::NonRoot,
            user,
            group,
            ..Default::default()
        }
    }

    pub fn custom() -> Self {
        Self {
            profile_type: SecurityProfileType::Custom,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRequirements {
    #[serde(default)]
    pub limits: ResourceList,
    #[serde(default)]
    pub requests: ResourceList,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceList {
    #[serde(default)]
    pub cpu: String,
    #[serde(default)]
    pub memory: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeSpec {
    #[serde(default)]
    pub liveness: ProbeConfig,
    #[serde(default)]
    pub readiness: ProbeConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeConfig {
    #[serde(default)]
    pub probe_type: ProbeType,
    #[serde(default)]
    pub path: String,
    /// Command to run for Exec probes. Ignored for Http/Tcp probe types.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "default_initial_delay")]
    pub initial_delay_seconds: i32,
    #[serde(default = "default_period")]
    pub period_seconds: i32,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: i32,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: i32,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            probe_type: ProbeType::Http,
            path: "/".to_string(),
            command: Vec::new(),
            initial_delay_seconds: 30,
            period_seconds: 10,
            timeout_seconds: 1,
            failure_threshold: 3,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub enum ProbeType {
    #[default]
    Http,
    Tcp,
    Exec,
}

fn default_initial_delay() -> i32 {
    30
}
fn default_period() -> i32 {
    10
}
fn default_timeout() -> i32 {
    1
}
fn default_failure_threshold() -> i32 {
    3
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeScheduling {
    #[serde(default)]
    pub node_selector: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    #[schemars(schema_with = "json_object_array_schema")]
    pub tolerations: Vec<serde_json::Value>,
    #[serde(default)]
    #[schemars(schema_with = "json_object_schema")]
    pub affinity: Option<serde_json::Value>,
}

fn default_true() -> bool {
    true
}

/// Configuration for the generated NetworkPolicy.
///
/// Controls egress rules (DNS, internet, private CIDRs) and ingress
/// from the gateway namespace. When omitted, the operator creates a
/// basic ingress-only policy on the app ports.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyConfig {
    /// Allow pods in the same namespace to reach this app (default: true).
    #[serde(default = "default_true")]
    pub allow_same_namespace: bool,
    /// Allow egress to kube-system DNS (UDP/TCP 53) (default: true).
    #[serde(default = "default_true")]
    pub allow_dns: bool,
    /// Allow egress to the public internet (default: false).
    #[serde(default)]
    pub allow_internet_egress: bool,
    /// CIDR blocks to deny in egress (e.g. RFC 1918 ranges).
    #[serde(default)]
    pub denied_cidr_blocks: Vec<String>,
    /// Arbitrary additional egress rules (raw NetworkPolicyEgressRule JSON).
    #[serde(default)]
    #[schemars(schema_with = "json_object_array_schema")]
    pub custom_egress_rules: Vec<serde_json::Value>,
}

impl Default for NetworkPolicyConfig {
    fn default() -> Self {
        Self {
            allow_same_namespace: true,
            allow_dns: true,
            allow_internet_egress: false,
            denied_cidr_blocks: Vec::new(),
            custom_egress_rules: Vec::new(),
        }
    }
}

/// Configuration for API-driven health checks.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiHealthCheckSpec {
    /// Whether API health checking is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// How often (in seconds) to poll the app API for health. Defaults to 60.
    #[serde(default)]
    pub interval_seconds: Option<u32>,
}

/// Backup configuration for the app.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupSpec {
    /// Whether automated backups are enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Cron expression for backup schedule (e.g. "0 3 * * *").
    #[serde(default)]
    pub schedule: String,
    /// Number of backups to retain.
    #[serde(default = "default_retention_count")]
    pub retention_count: u32,
}

fn default_retention_count() -> u32 {
    5
}

impl Default for BackupSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule: String::new(),
            retention_count: default_retention_count(),
        }
    }
}

/// GPU device passthrough configuration.
///
/// When set, the corresponding GPU device plugin resource is added
/// to the container's resource limits and requests.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GpuSpec {
    /// NVIDIA GPU count (adds `nvidia.com/gpu` resource limit+request).
    #[serde(default)]
    pub nvidia: Option<i32>,
    /// Intel iGPU count (adds `gpu.intel.com/i915` resource limit+request).
    #[serde(default)]
    pub intel: Option<i32>,
    /// AMD GPU count (adds `amd.com/gpu` resource limit+request).
    #[serde(default)]
    pub amd: Option<i32>,
}

/// Configuration for Prowlarr cross-app synchronization.
///
/// When enabled on a Prowlarr-type ServarrApp, the operator discovers
/// Sonarr/Radarr/Lidarr instances in the target namespace and registers
/// them as applications in Prowlarr for indexer sync.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrSyncSpec {
    /// Whether Prowlarr sync is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Namespace to discover apps in. Defaults to the Prowlarr CR's namespace.
    #[serde(default)]
    pub namespace_scope: Option<String>,
    /// Whether to remove apps from Prowlarr when their CRs are deleted.
    #[serde(default = "default_true")]
    pub auto_remove: bool,
}

impl Default for ProwlarrSyncSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            namespace_scope: None,
            auto_remove: true,
        }
    }
}

/// Configuration for Overseerr cross-app synchronization.
///
/// When enabled on an Overseerr-type ServarrApp, the operator discovers
/// Sonarr/Radarr instances in the target namespace and registers them as
/// servers in Overseerr with correct `is4k`/`isDefault` flags.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OverseerrSyncSpec {
    /// Whether Overseerr sync is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Namespace to discover apps in. Defaults to the Overseerr CR's namespace.
    #[serde(default)]
    pub namespace_scope: Option<String>,
    /// Whether to remove servers from Overseerr when their CRs are deleted.
    #[serde(default = "default_true")]
    pub auto_remove: bool,
}

impl Default for OverseerrSyncSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            namespace_scope: None,
            auto_remove: true,
        }
    }
}

/// Sync spec for Bazarr → Sonarr/Radarr integration.
///
/// When enabled on a Bazarr-type ServarrApp, the operator discovers
/// Sonarr/Radarr instances in the target namespace and registers them
/// in Bazarr for subtitle management.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BazarrSyncSpec {
    /// Enable Bazarr cross-app sync.
    #[serde(default)]
    pub enabled: bool,
    /// Namespace to discover companion apps in. Defaults to Bazarr's own namespace.
    #[serde(default)]
    pub namespace_scope: Option<String>,
    /// Remove Sonarr/Radarr registrations from Bazarr when their CRs disappear.
    #[serde(default = "default_true")]
    pub auto_remove: bool,
}

impl Default for BazarrSyncSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            namespace_scope: None,
            auto_remove: true,
        }
    }
}

/// Sync spec for Subgen → Jellyfin integration.
///
/// When enabled on a Subgen-type ServarrApp, the operator discovers
/// Jellyfin instances in the target namespace and registers them in
/// Subgen for subtitle generation.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubgenSyncSpec {
    /// Enable Subgen cross-app sync with Jellyfin.
    #[serde(default)]
    pub enabled: bool,
    /// Namespace to discover Jellyfin in. Defaults to Subgen's own namespace.
    #[serde(default)]
    pub namespace_scope: Option<String>,
}

/// Configuration for the in-cluster NFS server deployed by the MediaStack operator.
///
/// By default (when this field is absent or `enabled` is true), the operator
/// deploys an in-cluster NFS server backed by a PVC and auto-injects NFS mounts
/// into every app in the stack.
///
/// Set `enabled: false` to disable the in-cluster server entirely, or set
/// `externalServer` to use your own NFS server instead of deploying one.
/// Setting `externalServer` implicitly disables the in-cluster server.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NfsServerSpec {
    /// Deploy an in-cluster NFS server. Defaults to true. Ignored when
    /// `externalServer` is set.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Size of the PVC backing the in-cluster NFS server. Defaults to "1Ti".
    #[serde(default = "default_nfs_storage_size")]
    pub storage_size: String,

    /// Storage class for the NFS server PVC. If omitted, uses the cluster default.
    #[serde(default)]
    pub storage_class: Option<String>,

    /// Image override for the NFS server container.
    #[serde(default)]
    pub image: Option<ImageSpec>,

    /// Subpath within the NFS share for movies, and the container mount path.
    /// Defaults to "/movies".
    ///
    /// For in-cluster NFS the server-side path equals `moviesPath` (the
    /// in-cluster export root is `/nfsshare` with `fsid=0`, so `/movies` is
    /// the full server path).
    /// For external NFS the server-side path is `{externalPath}{moviesPath}`.
    #[serde(default = "default_movies_path")]
    pub movies_path: String,

    /// Subpath within the NFS share for TV shows, and the container mount path.
    /// Defaults to "/tv".
    #[serde(default = "default_tv_path")]
    pub tv_path: String,

    /// Subpath within the NFS share for music, and the container mount path.
    /// Defaults to "/music".
    #[serde(default = "default_music_path")]
    pub music_path: String,

    /// Subpath for 4K movies (used when split4k is enabled). Defaults to "/movies-4k".
    /// The 4K Radarr instance mounts this path at the same container path as the
    /// standard instance (`moviesPath`), so app configuration is unchanged.
    #[serde(default = "default_movies_4k_path")]
    pub movies_4k_path: String,

    /// Subpath for 4K TV shows (used when split4k is enabled). Defaults to "/tv-4k".
    /// The 4K Sonarr instance mounts this at the same container path as the standard
    /// instance (`tvPath`), so app configuration is unchanged.
    #[serde(default = "default_tv_4k_path")]
    pub tv_4k_path: String,

    /// Address of an external NFS server to use instead of deploying one in-cluster.
    /// When set, no NFS server resources are created and this address is used for
    /// all auto-injected NFS mounts. Mutually exclusive with `enabled: true`.
    #[serde(default)]
    pub external_server: Option<String>,

    /// Root export path on the external NFS server. Defaults to "/". The media
    /// subpath fields (`moviesPath`, `tvPath`, etc.) are appended to this root
    /// to form the NFS server-side path (e.g. `/volume1` + `/movies` = `/volume1/movies`).
    #[serde(default = "default_external_path")]
    pub external_path: String,
}

impl Default for NfsServerSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            storage_size: default_nfs_storage_size(),
            storage_class: None,
            image: None,
            movies_path: default_movies_path(),
            tv_path: default_tv_path(),
            music_path: default_music_path(),
            movies_4k_path: default_movies_4k_path(),
            tv_4k_path: default_tv_4k_path(),
            external_server: None,
            external_path: default_external_path(),
        }
    }
}

impl NfsServerSpec {
    /// Returns true if an in-cluster NFS server should be deployed.
    pub fn deploy_in_cluster(&self) -> bool {
        self.external_server.is_none() && self.enabled
    }

    /// Returns the NFS server address to use in volume mounts.
    ///
    /// For in-cluster servers, provide `stack_name` and `namespace` to derive
    /// the cluster-local DNS name. For external servers, returns the configured
    /// `external_server` address.
    pub fn server_address(&self, stack_name: &str, namespace: &str) -> Option<String> {
        if let Some(ref ext) = self.external_server {
            Some(ext.clone())
        } else if self.enabled {
            Some(format!(
                "{stack_name}-nfs-server.{namespace}.svc.cluster.local"
            ))
        } else {
            None
        }
    }

    /// Compute the NFS server-side path for a given media subpath.
    ///
    /// For in-cluster servers `/nfsshare` is the NFSv4 root (`fsid=0`), so
    /// clients mount paths relative to it: `nfs_path("/movies")` → `"/movies"`.
    ///
    /// For external servers the export root is `external_path`, so
    /// `nfs_path("/movies")` with `external_path="/volume1"` → `"/volume1/movies"`.
    pub fn nfs_path(&self, media_subpath: &str) -> String {
        if self.external_server.is_some() {
            let root = self.external_path.trim_end_matches('/');
            if root.is_empty() || root == "/" {
                media_subpath.to_string()
            } else {
                format!("{root}{media_subpath}")
            }
        } else {
            media_subpath.to_string()
        }
    }
}

fn default_nfs_storage_size() -> String {
    "1Ti".to_string()
}

fn default_movies_path() -> String {
    "/movies".to_string()
}

fn default_tv_path() -> String {
    "/tv".to_string()
}

fn default_music_path() -> String {
    "/music".to_string()
}

fn default_movies_4k_path() -> String {
    "/movies-4k".to_string()
}

fn default_tv_4k_path() -> String {
    "/tv-4k".to_string()
}

fn default_external_path() -> String {
    "/".to_string()
}

/// Reference to a user-created Kubernetes Secret containing admin credentials.
///
/// The operator reads but never creates or owns this secret. It must have
/// `username` and `password` keys.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminCredentialsSpec {
    /// Name of a Kubernetes Secret containing `username` and `password` keys.
    pub secret_name: String,
}

fn json_object_schema(_gen: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "object" })
}

fn json_object_array_schema(_gen: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "items": { "type": "object" }
    })
}
