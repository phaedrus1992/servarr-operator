use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AppConfig {
    Transmission(TransmissionConfig),
    Sabnzbd(SabnzbdConfig),
    Prowlarr(ProwlarrConfig),
    SshBastion(SshBastionConfig),
    #[serde(alias = "overseerr")]
    Seerr(Box<SeerrConfig>),
    Lidarr(LidarrConfig),
    Unpackerr(UnpackerrConfig),
}

// --- Unpackerr ---

/// One *arr instance for Unpackerr's static `/config/unpackerr.conf` (#604).
///
/// Unpackerr has no live API of its own, so its *arr connections are seeded
/// once by an init container from these CRD spec fields rather than
/// auto-discovered on every reconcile (contrast `CleanuparrSyncSpec`,
/// `HoundarrSyncSpec`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnpackerrArrInstance {
    /// Base URL of the *arr instance, e.g. `http://sonarr.media.svc:8989`.
    pub url: String,
    /// Secret containing this instance's API key (key: `api-key`).
    pub api_key_secret: String,
}

/// Unpackerr configuration: one optional instance per supported *arr kind.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnpackerrConfig {
    #[serde(default)]
    pub sonarr: Option<UnpackerrArrInstance>,
    #[serde(default)]
    pub radarr: Option<UnpackerrArrInstance>,
    #[serde(default)]
    pub lidarr: Option<UnpackerrArrInstance>,
    #[serde(default)]
    pub readarr: Option<UnpackerrArrInstance>,
}

// --- Prowlarr ---

/// Custom indexer definition for Prowlarr.
///
/// Each definition becomes a YAML file placed in
/// `/config/Definitions/Custom/{name}.yml` inside the Prowlarr container.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexerDefinition {
    /// Filename (without extension) for the definition. Must be alphanumeric
    /// with optional hyphens (e.g. `my-private-tracker`).
    pub name: String,
    /// The YAML body of the Prowlarr indexer definition.
    pub content: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrConfig {
    /// Custom indexer definitions to place in /config/Definitions/Custom.
    #[serde(default)]
    pub custom_definitions: Vec<IndexerDefinition>,
}

// --- SABnzbd ---

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SabnzbdConfig {
    /// Hostnames that SABnzbd should accept connections from.
    /// Required for reverse proxy setups (e.g. `["sonarr.example.com"]`).
    #[serde(default)]
    pub host_whitelist: Vec<String>,
    /// Enable automatic tar/archive unpacking after downloads complete.
    /// Installs compression tools (tar, gzip, bzip2, xz, zstd) and adds
    /// a post-processing script.
    #[serde(default)]
    pub tar_unpack: bool,
}

// --- Transmission ---

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransmissionConfig {
    #[serde(default = "default_settings")]
    #[schemars(schema_with = "json_object_schema")]
    pub settings: serde_json::Value,
    #[serde(default)]
    pub peer_port: Option<PeerPortConfig>,
    #[serde(default)]
    pub auth: Option<TransmissionAuth>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeerPortConfig {
    pub port: i32,
    #[serde(default)]
    pub host_port: bool,
    #[serde(default)]
    pub random_on_start: bool,
    #[serde(default = "default_random_low")]
    pub random_low: i32,
    #[serde(default = "default_random_high")]
    pub random_high: i32,
}

fn default_random_low() -> i32 {
    49152
}
fn default_random_high() -> i32 {
    65535
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransmissionAuth {
    pub secret_name: String,
}

fn json_object_schema(_gen: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "object", "x-kubernetes-preserve-unknown-fields": true })
}

fn default_settings() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

// --- SSH Bastion ---

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SshBastionConfig {
    /// SSH users to provision on the bastion.
    #[serde(default)]
    pub users: Vec<SshUser>,

    /// Whether to allow password authentication (default: false).
    #[serde(default)]
    pub enable_password_auth: bool,

    /// Whether to allow TCP forwarding (default: false).
    #[serde(default)]
    pub tcp_forwarding: bool,

    /// Whether to allow gateway ports (default: false).
    #[serde(default)]
    pub gateway_ports: bool,

    /// Message of the day shown on login.
    #[serde(default)]
    pub motd: String,

    /// Disable SFTP subsystem (default: false).
    #[serde(default)]
    pub disable_sftp: bool,

    /// SFTP chroot directory (default: "%h" for user home).
    #[serde(default = "default_sftp_chroot")]
    pub sftp_chroot: String,
}

fn default_sftp_chroot() -> String {
    "%h".to_string()
}

/// An SSH user to provision on the bastion.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SshUser {
    /// Username.
    pub name: String,
    /// User ID.
    pub uid: i64,
    /// Group ID.
    pub gid: i64,
    /// SSH access mode for this user: shell, sftp, scp, rsync, or restricted-rsync.
    #[serde(default)]
    pub mode: SshMode,
    /// Restricted rsync configuration (only applies when mode is restricted-rsync).
    #[serde(default)]
    pub restricted_rsync: Option<RestrictedRsyncConfig>,
    /// Override login shell (only applies when mode is shell; default: /bin/sh).
    #[serde(default)]
    pub shell: Option<String>,
    /// SSH public keys (one per line).
    #[serde(default)]
    pub public_keys: String,
}

/// SSH access mode.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SshMode {
    /// Full interactive shell access.
    #[default]
    Shell,
    /// SFTP only.
    Sftp,
    /// SCP only.
    Scp,
    /// Rsync only.
    Rsync,
    /// Restricted rsync with path and read-only controls.
    RestrictedRsync,
}

/// Configuration for restricted-rsync mode.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RestrictedRsyncConfig {
    /// Paths that users are allowed to rsync from.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
}

// --- Seerr ---

/// Seerr integration configuration.
///
/// Provides default Sonarr and Radarr server settings used when the operator
/// auto-registers discovered instances in Seerr.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeerrConfig {
    /// Default Sonarr server settings for Seerr registration.
    #[serde(default)]
    pub sonarr: Option<SeerrServerDefaults>,
    /// Default Radarr server settings for Seerr registration.
    #[serde(default)]
    pub radarr: Option<SeerrServerDefaults>,
}

/// Default settings applied when registering a Sonarr or Radarr server in Seerr.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeerrServerDefaults {
    /// Quality profile ID to set as active in Seerr.
    pub profile_id: f64,
    /// Quality profile name.
    pub profile_name: String,
    /// Root folder path (e.g. "/movies", "/tv").
    pub root_folder: String,
    /// Minimum availability for Radarr (e.g. "released"). Ignored for Sonarr.
    #[serde(default)]
    pub minimum_availability: Option<String>,
    /// Enable season folders (Sonarr only).
    #[serde(default)]
    pub enable_season_folders: Option<bool>,
    /// 4K variant overrides (used when the server is a 4K instance).
    #[serde(default)]
    pub four_k: Option<SeerrServerDefaults4k>,
}

/// Override settings for 4K instances registered in Seerr.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeerrServerDefaults4k {
    /// Quality profile ID for the 4K instance.
    pub profile_id: f64,
    /// Quality profile name for the 4K instance.
    pub profile_name: String,
    /// Root folder path for the 4K instance (e.g. "/movies4k").
    pub root_folder: String,
    /// Minimum availability for the 4K Radarr instance.
    #[serde(default)]
    pub minimum_availability: Option<String>,
    /// Enable season folders for the 4K Sonarr instance.
    #[serde(default)]
    pub enable_season_folders: Option<bool>,
}

// --- Lidarr ---

/// Configuration for the Lidarr application.
//
// This stays an empty braced struct (not a unit struct) so existing CRs that
// still carry the legacy sidecar config key keep deserializing — serde drops
// the unknown key. Keep this note as a `//` comment, not `///`: a doc comment
// would leak the internal rationale into the generated CRD schema description.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::empty_structs_with_brackets,
    reason = "braced form deserializes from a map; a unit struct would reject existing `{lidarr: {…}}` CRs carrying the legacy sidecar key"
)]
pub struct LidarrConfig {}
