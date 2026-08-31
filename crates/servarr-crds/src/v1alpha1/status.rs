use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use servarr_api::TenantSafeMessage;

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServarrAppStatus {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub ready_replicas: i32,
    #[serde(default)]
    pub observed_generation: i64,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub backup_status: Option<BackupStatus>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupStatus {
    pub last_backup_time: Option<String>,
    pub last_backup_result: Option<String>,
    #[serde(default)]
    pub backup_count: u32,
}

impl ServarrAppStatus {
    /// Set or update a condition by type. If a condition with the same type
    /// already exists, update it in place; otherwise append it.
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

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub condition_type: String,
    pub status: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub last_transition_time: String,
}

/// Well-known condition types for ServarrApp status.
pub mod condition_types {
    pub const READY: &str = "Ready";
    pub const DEPLOYMENT_READY: &str = "DeploymentReady";
    pub const SERVICE_READY: &str = "ServiceReady";
    pub const NETWORK_POLICY_READY: &str = "NetworkPolicyReady";
    pub const ROUTE_READY: &str = "RouteReady";
    pub const PVC_READY: &str = "PvcReady";
    pub const PROGRESSING: &str = "Progressing";
    pub const DEGRADED: &str = "Degraded";
    pub const APP_HEALTHY: &str = "AppHealthy";
    pub const UPDATE_AVAILABLE: &str = "UpdateAvailable";
    pub const ADMIN_CREDENTIALS_CONFIGURED: &str = "AdminCredentialsConfigured";
    /// Cross-app sync health for Bazarr (True = last sync OK, False = last sync failed).
    pub const BAZARR_SYNC_READY: &str = "BazarrSyncReady";
    /// Cross-app sync health for Subgen → Jellyfin (True = OK, False = failed).
    pub const SUBGEN_SYNC_READY: &str = "SubgenSyncReady";
    /// Cross-app sync health for Prowlarr (True = last sync OK, False = last sync failed).
    pub const PROWLARR_SYNC_READY: &str = "ProwlarrSyncReady";
    /// Cross-app sync health for Seerr (True = last sync OK, False = last sync failed).
    pub const SEERR_SYNC_READY: &str = "SeerrSyncReady";
    /// Cross-app sync health for Maintainerr (True = last sync OK, False = last sync failed).
    pub const MAINTAINERR_SYNC_READY: &str = "MaintainerrSyncReady";
    /// Cross-app sync health for Cleanuparr (True = last sync OK, False = last sync failed).
    pub const CLEANUPARR_SYNC_READY: &str = "CleanuparrSyncReady";
    /// Cross-app sync health for Houndarr (True = last sync OK, False = last sync failed).
    pub const HOUNDARR_SYNC_READY: &str = "HoundarrSyncReady";
    /// Backup restore result (True = restore succeeded, False = restore failed).
    pub const RESTORE_READY: &str = "RestoreReady";
    /// Transmission download-client data health (True = no torrents reporting missing
    /// data, False = one or more torrents reference on-disk data that has gone missing).
    pub const DOWNLOAD_DATA_HEALTHY: &str = "DownloadDataHealthy";
}

impl Condition {
    /// Create a True condition.
    ///
    /// `message` must be a [`TenantSafeMessage`] (or convert into one), the same as
    /// [`Condition::fail`] (#709). A success message is tenant-visible too, so it carries the
    /// same compiler-enforced guarantee: a raw `&str` cannot satisfy
    /// `impl Into<TenantSafeMessage>`.
    ///
    /// ```compile_fail
    /// use servarr_crds::Condition;
    /// // A raw &str must not silently satisfy `impl Into<TenantSafeMessage>`:
    /// let _ = Condition::ok("Ready", "Succeeded", "raw untrusted string", "now");
    /// ```
    pub fn ok(
        condition_type: &str,
        reason: &str,
        message: impl Into<TenantSafeMessage>,
        now: &str,
    ) -> Self {
        Self {
            condition_type: condition_type.to_string(),
            status: "True".to_string(),
            reason: reason.to_string(),
            message: message.into().to_string(),
            last_transition_time: now.to_string(),
        }
    }

    /// Create a False condition.
    ///
    /// `message` must be a [`TenantSafeMessage`] (or convert into one) -- this is the
    /// compiler-enforced version of the sanitized-string guarantee: a raw `&str` cannot
    /// satisfy `impl Into<TenantSafeMessage>` (#668), so only sanitizer output or an
    /// explicit `TenantSafeMessage::new` call can ever reach a tenant-visible Condition.
    ///
    /// ```compile_fail
    /// use servarr_crds::Condition;
    /// // A raw &str must not silently satisfy `impl Into<TenantSafeMessage>`:
    /// let _ = Condition::fail("Ready", "Failed", "raw untrusted string", "now");
    /// ```
    pub fn fail(
        condition_type: &str,
        reason: &str,
        message: impl Into<TenantSafeMessage>,
        now: &str,
    ) -> Self {
        Self {
            condition_type: condition_type.to_string(),
            status: "False".to_string(),
            reason: reason.to_string(),
            message: message.into().to_string(),
            last_transition_time: now.to_string(),
        }
    }

    /// Create an Unknown condition.
    ///
    /// An `Unknown` message is tenant-visible, so it carries the same
    /// [`TenantSafeMessage`] requirement as [`Condition::ok`] and [`Condition::fail`].
    /// This exists so the three status values share one gate: a caller that needs
    /// `Unknown` no longer has to build the struct directly and skip the check.
    ///
    /// ```compile_fail
    /// use servarr_crds::Condition;
    /// // A raw &str must not silently satisfy `impl Into<TenantSafeMessage>`:
    /// let _ = Condition::unknown("Ready", "Unclear", "raw untrusted string", "now");
    /// ```
    pub fn unknown(
        condition_type: &str,
        reason: &str,
        message: impl Into<TenantSafeMessage>,
        now: &str,
    ) -> Self {
        Self {
            condition_type: condition_type.to_string(),
            status: "Unknown".to_string(),
            reason: reason.to_string(),
            message: message.into().to_string(),
            last_transition_time: now.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_accepts_a_tenant_safe_message_and_preserves_its_text() {
        let condition = Condition::unknown(
            "DownloadDataHealthy",
            "TorrentGetError",
            TenantSafeMessage::new("curated unknown text"),
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(condition.message, "curated unknown text");
        assert_eq!(condition.status, "Unknown");
        assert_eq!(condition.reason, "TorrentGetError");
    }

    #[test]
    fn ok_accepts_a_tenant_safe_message_and_preserves_its_text() {
        let condition = Condition::ok(
            "Ready",
            "SyncSucceeded",
            TenantSafeMessage::new("curated success text"),
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(condition.message, "curated success text");
        assert_eq!(condition.status, "True");
        assert_eq!(condition.reason, "SyncSucceeded");
    }

    #[test]
    fn fail_accepts_a_tenant_safe_message_and_preserves_its_text() {
        let condition = Condition::fail(
            "Ready",
            "SyncFailed",
            TenantSafeMessage::new("curated failure text"),
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(condition.message, "curated failure text");
        assert_eq!(condition.status, "False");
        assert_eq!(condition.reason, "SyncFailed");
    }
}
