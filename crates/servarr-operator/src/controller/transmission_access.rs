//! Transmission client and adminCredentials resolution, shared by the read-only health probe
//! and the destructive download-health pass (#483, #499, #846).

use kube::{Client, ResourceExt};
use servarr_crds::ServarrApp;
use tracing::warn;

/// Read Transmission's optional adminCredentials secret for RPC authentication. A missing
/// secret or unreadable key logs a warning and falls back to unauthenticated access rather
/// than failing the caller outright.
async fn read_transmission_admin_creds(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
) -> AdminCredsRead {
    let Some(ref ac) = app.spec.admin_credentials else {
        return AdminCredsRead::default();
    };
    let mut read = AdminCredsRead::default();
    match servarr_api::read_secret_key(client, ns, &ac.secret_name, "username").await {
        Ok(v) => read.username = Some(v),
        Err(e) => {
            warn!(app = %app.name_any(), error = %e.log_summary(),
                    "health-check: failed to read Transmission username, proceeding unauthenticated");
            read.record_error(e.public_summary());
        }
    }
    match servarr_api::read_secret_key(client, ns, &ac.secret_name, "password").await {
        Ok(v) => read.password = Some(v),
        Err(e) => {
            warn!(app = %app.name_any(), error = %e.log_summary(),
                    "health-check: failed to read Transmission password, proceeding unauthenticated");
            read.record_error(e.public_summary());
        }
    }
    read
}

/// Outcome of reading Transmission's adminCredentials secret. `read_error` keeps the first
/// failure's tenant-safe summary, so a status condition can name the category (RBAC denial,
/// missing key, apiserver timeout) instead of blaming the secret contents for every failure.
#[derive(Debug, Default)]
struct AdminCredsRead {
    username: Option<String>,
    password: Option<String>,
    read_error: Option<String>,
}

impl AdminCredsRead {
    fn record_error(&mut self, summary: String) {
        self.read_error.get_or_insert(summary);
    }
}

/// Transmission client + admin-credential state resolved once per reconcile pass and shared
/// between `check_api_health`'s Transmission arm and `check_download_client_health` — both gate
/// on the same `apiHealthCheck.enabled` flag, so building this independently in each would mean
/// double the Secret GETs and a redundant session-ID handshake against the same app (#499).
#[derive(Debug)]
pub(crate) struct TransmissionAccess {
    pub(super) client: servarr_api::TransmissionClient,
    /// `true` when `adminCredentials` was configured but at least one of username/password
    /// could not be read (partial or total read failure). The read-only health probe below
    /// degrades to unauthenticated in this case; the destructive download-health path fails
    /// closed instead (#483 hardening) — this flag lets each caller apply its own policy
    /// against the same resolved credentials.
    pub(super) credentials_incomplete: bool,
    /// Tenant-safe reason the adminCredentials read failed, when one did. Lets the destructive
    /// path's status condition name the category (RBAC denial, timeout, missing key) (#846).
    pub(super) credentials_error: Option<String>,
}

/// Resolve a [`TransmissionAccess`] for `app`: read the optional adminCredentials secret and
/// build a `TransmissionClient` against `base_url`. Called once per reconcile (see `reconcile`)
/// so both health checks share the same client instance instead of each triggering their own
/// session-ID handshake (#499).
pub(super) async fn resolve_transmission_access(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    base_url: &str,
) -> Result<TransmissionAccess, String> {
    let AdminCredsRead {
        username: tx_user,
        password: tx_pass,
        read_error: credentials_error,
    } = read_transmission_admin_creds(client, app, ns).await;
    // Covers both a partial read (one of username/password missing) and a total read failure
    // (both missing, e.g. the secret was deleted or renamed) -- either way adminCredentials was
    // configured but couldn't be fully honored, so the destructive path below must not proceed
    // unauthenticated on the strength of an unrelated coincidence (both `None`).
    let credentials_incomplete =
        app.spec.admin_credentials.is_some() && !(tx_user.is_some() && tx_pass.is_some());
    // Half-set credentials are unrepresentable: build the Basic-auth pair only when both
    // halves are present, else degrade to an anonymous client. `credentials_incomplete`
    // above still records the partial-read case so the destructive path can fail closed (#505).
    let tx_credentials = match (tx_user.as_deref(), tx_pass.as_deref()) {
        (Some(u), Some(p)) => Some(servarr_api::BasicCredentials::new(u, p)),
        _ => None,
    };
    let tx_client = servarr_api::TransmissionClient::new(base_url, tx_credentials.as_ref())
        .map_err(|e| e.log_summary())?;
    Ok(TransmissionAccess {
        client: tx_client,
        credentials_incomplete,
        credentials_error,
    })
}

#[cfg(test)]
mod credential_read_tests {
    use super::AdminCredsRead;

    #[test]
    fn record_error_keeps_first_failure() {
        let mut read = AdminCredsRead::default();
        read.record_error("RBAC denied".to_string());
        read.record_error("timed out".to_string());
        assert_eq!(read.read_error.as_deref(), Some("RBAC denied"));
    }
}
