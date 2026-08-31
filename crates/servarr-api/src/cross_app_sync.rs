//! Shared behavior for companion apps that auto-register discovered *arr instances.
//!
//! Maintainerr, Cleanuparr, and Houndarr each expose a way to list and register
//! Sonarr/Radarr/Lidarr/Readarr instances, so the reconcile-side "list what's
//! registered, then register anything discovered but missing" logic
//! (`sync_cross_app` in `servarr-operator`) is written once against this trait
//! instead of once per client.
//!
//! `kind` is a plain `&str` (`"sonarr"`, `"radarr"`, `"lidarr"`, `"readarr"`), not
//! `servarr_crds::AppType` — this crate sits below `servarr-crds` in the
//! dependency graph and must not depend on it.

use crate::client::ApiError;

/// One *arr instance as a companion app models it in its own registration API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredArrInstance {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
}

/// List and register *arr instances in a companion app.
pub trait CrossAppSync {
    /// List the names of instances of one *arr `kind` already registered in the
    /// companion app.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` if the request fails, the companion app returns a
    /// non-success response, or `kind` is not one this companion app supports.
    fn list_registered(
        &self,
        kind: &str,
    ) -> impl Future<Output = Result<Vec<String>, ApiError>> + Send;

    /// Register one *arr instance of the given `kind`.
    ///
    /// Callers only call this for a name absent from [`list_registered`](Self::list_registered)
    /// — this method does not de-duplicate.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` if the request fails, the companion app returns a
    /// non-success response, or `kind` is not one this companion app supports.
    fn register(
        &self,
        kind: &str,
        instance: &RegisteredArrInstance,
    ) -> impl Future<Output = Result<(), ApiError>> + Send;
}
