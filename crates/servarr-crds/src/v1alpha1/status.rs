use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
}

impl Condition {
    /// Create a True condition.
    pub fn ok(condition_type: &str, reason: &str, message: &str, now: &str) -> Self {
        Self {
            condition_type: condition_type.to_string(),
            status: "True".to_string(),
            reason: reason.to_string(),
            message: message.to_string(),
            last_transition_time: now.to_string(),
        }
    }

    /// Create a False condition.
    pub fn fail(condition_type: &str, reason: &str, message: &str, now: &str) -> Self {
        Self {
            condition_type: condition_type.to_string(),
            status: "False".to_string(),
            reason: reason.to_string(),
            message: message.to_string(),
            last_transition_time: now.to_string(),
        }
    }
}
