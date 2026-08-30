use kube::api::Api;
use kube::runtime::events::{Event, EventType, Recorder};
use kube::{Client, ResourceExt};
use servarr_api::TenantSafeMessage;
use servarr_api::k8s::{is_kube_not_found, is_kube_permission_denied, kube_err_summary};
use servarr_crds::{AppType, ServarrApp};
use tracing::{info, warn};

/// Whether a cleanup failure proves the downstream target is already gone (`Terminal` — safe to
/// treat as idempotent success, since retrying can never make an absent target more absent) or
/// might still succeed on a later attempt (`Transient` — must keep the finalizer so the cleanup
/// is retried, never silently dropped). See #451.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CleanupSeverity {
    Terminal,
    Transient,
}

/// For a `Transient` cleanup failure, whether retrying is expected to eventually succeed on its
/// own (#669, same shape as #610's `DetachFailureCause`): a permission failure (401/403, a bad
/// API key) needs a manual fix and will retry forever without one, while a genuine 5xx/network
/// blip may clear on its own. `Terminal` failures don't need this distinction -- they already
/// stop retrying.
///
/// Deliberately doesn't change `finish_cleanup`'s retry/drop-finalizer decision: a `Transient`
/// permission failure still keeps the finalizer and retries, same as before this was added.
/// Silently dropping the finalizer because of an RBAC/API-key problem would abandon the
/// downstream Prowlarr/Seerr cleanup outright, which is worse than retrying forever with a
/// clearer diagnostic. This only enriches the `CleanupFailed` Event/log so on-call can tell
/// "will probably clear on its own" from "needs a human to fix a credential."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RetryOutlook {
    /// May succeed on a later attempt without intervention.
    MaySelfResolve,
    /// Needs a manual fix (credential, RBAC, API key, malformed config) -- will retry forever
    /// without one.
    NeedsManualFix,
}

/// Classifies an error's [`CleanupSeverity`] and, for `Transient` failures, its [`RetryOutlook`].
/// Implemented only for the concrete error types the cleanup path actually produces
/// (`kube::Error`, `SecretError`, `ApiError`) — deliberately no blanket/default impl for either
/// method, so a new error type flowing through [`CleanupMapErr`] must get an explicit, reviewed
/// classification rather than silently defaulting to one outcome or the other.
pub(super) trait ClassifyCleanupSeverity {
    fn cleanup_severity(&self) -> CleanupSeverity;
    fn retry_outlook(&self) -> RetryOutlook;
}

impl ClassifyCleanupSeverity for kube::Error {
    fn cleanup_severity(&self) -> CleanupSeverity {
        // The API server has no such object (Secret, ServarrApp, ...) — provably absent. Uses
        // the shared `is_kube_not_found` predicate (see #659/#660) so the underlying 404 check
        // stays in one place; the Terminal/Transient retry duality built on top of it here is
        // specific to the finalizer-cleanup path and isn't shared further.
        if is_kube_not_found(self) {
            CleanupSeverity::Terminal
        } else {
            CleanupSeverity::Transient
        }
    }

    fn retry_outlook(&self) -> RetryOutlook {
        // Matched exhaustively (kube::Error is not #[non_exhaustive] as of kube 3.1.0) rather
        // than an if/else with an implicit catch-all: a boolean check that only special-cased
        // permission errors would silently default every other variant to MaySelfResolve,
        // exactly the "new error type defaults to one outcome silently" failure mode this
        // trait's own doc comment says not to allow. If `kube` adds a variant, this becomes a
        // compile error here instead of a silent misclassification.
        match self {
            kube::Error::Api(_) if is_kube_permission_denied(self) => RetryOutlook::NeedsManualFix,
            // Any other 4xx (except 429, which is genuinely retry-friendly -- rate limiting)
            // means the request itself is malformed or otherwise rejected; retrying an
            // identical request gets the identical rejection every time.
            kube::Error::Api(status) if (400..500).contains(&status.code) && status.code != 429 => {
                RetryOutlook::NeedsManualFix
            }
            // 5xx, 429, or an unusual/unmapped code -- treat as a server-side condition that
            // can plausibly clear on its own.
            kube::Error::Api(_) => RetryOutlook::MaySelfResolve,
            // Credential/exec-plugin, proxy/TLS, kubeconfig, and API-discovery/schema problems
            // (Discovery wraps DiscoveryError's own InvalidGroupVersion/MissingKind/
            // MissingApiGroup/MissingResource/EmptyApiGroup variants) are all static config
            // issues: deterministic given the same input, so retrying changes nothing without
            // a human fixing the underlying config.
            kube::Error::Auth(_)
            | kube::Error::RustlsTls(_)
            | kube::Error::TlsRequired
            | kube::Error::ProxyProtocolUnsupported { .. }
            | kube::Error::ProxyProtocolDisabled { .. }
            | kube::Error::InferConfig(_)
            | kube::Error::InferKubeconfig(_)
            | kube::Error::Discovery(_)
            | kube::Error::SerdeError(_)
            | kube::Error::BuildRequest(_)
            | kube::Error::HttpError(_)
            | kube::Error::FromUtf8(_)
            | kube::Error::LinesCodecMaxLineLengthExceeded => RetryOutlook::NeedsManualFix,
            // Transport/connection-level -- a network blip that can plausibly clear on its own.
            kube::Error::HyperError(_) | kube::Error::Service(_) | kube::Error::ReadEvents(_) => {
                RetryOutlook::MaySelfResolve
            }
        }
    }
}

impl ClassifyCleanupSeverity for servarr_api::k8s::SecretError {
    fn cleanup_severity(&self) -> CleanupSeverity {
        match self {
            Self::Kube(e) => e.cleanup_severity(),
            // The Secret exists but is missing data/the key, or the value isn't UTF-8 — a
            // configuration problem, not proof the downstream state is absent. Keep retrying:
            // an operator fixing the Secret shouldn't need the app re-deleted to unstick cleanup.
            Self::NoData { .. } | Self::KeyNotFound { .. } | Self::InvalidUtf8 { .. } => {
                CleanupSeverity::Transient
            }
        }
    }

    fn retry_outlook(&self) -> RetryOutlook {
        match self {
            Self::Kube(e) => e.retry_outlook(),
            // Same reasoning as `cleanup_severity` above: this needs a human to fix the
            // Secret's contents, not a passive retry.
            Self::NoData { .. } | Self::KeyNotFound { .. } | Self::InvalidUtf8 { .. } => {
                RetryOutlook::NeedsManualFix
            }
        }
    }
}

impl ClassifyCleanupSeverity for servarr_api::ApiError {
    fn cleanup_severity(&self) -> CleanupSeverity {
        match self {
            // The downstream *arr app returned 404 for the registration/instance we tried to
            // read or delete — already gone.
            Self::ApiResponse { status: 404, .. } => CleanupSeverity::Terminal,
            _ => CleanupSeverity::Transient,
        }
    }

    fn retry_outlook(&self) -> RetryOutlook {
        match self {
            // Any 4xx from the downstream *arr app (except 429, genuinely retry-friendly rate
            // limiting) means the request was rejected -- an identical retry gets an identical
            // rejection, whether that's 401/403 (bad apiKeySecret) or 400/422 (malformed
            // request). An API key the client itself already rejected as malformed, or a
            // malformed base URL, are the same static-config-problem class one layer earlier.
            Self::ApiResponse { status, .. } if (400..500).contains(status) && *status != 429 => {
                RetryOutlook::NeedsManualFix
            }
            Self::InvalidApiKey | Self::InvalidUrl(_) => RetryOutlook::NeedsManualFix,
            // `Request` (transport-level: DNS, connect, timeout) and `OperationFailed` (the
            // downstream app rejected the operation with a free-text message we can't reliably
            // classify further) are left as may-self-resolve, matching the prior undifferentiated
            // behavior for both. Any other ApiResponse status (5xx, 429, or unmapped) is a
            // server-side condition that can plausibly clear on its own.
            Self::ApiResponse { .. } | Self::Request(_) | Self::OperationFailed { .. } => {
                RetryOutlook::MaySelfResolve
            }
        }
    }
}

/// Remove this app's registration from Prowlarr when the CR is deleted.
///
/// See [`finish_cleanup`] for how the `Terminal`/`Transient` outcome of the cleanup body maps to
/// this function's return value and Event publication.
pub(crate) async fn cleanup_prowlarr_registration(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    base_url_override: Option<&str>,
) -> Result<(), anyhow::Error> {
    let outcome = cleanup_prowlarr_registration_body(
        client,
        app,
        namespace,
        recorder,
        obj_ref,
        base_url_override,
    )
    .await;
    finish_cleanup(outcome, "Prowlarr", app, recorder, obj_ref).await
}

/// A cleanup body's failure: the `error` side carries full detail for the operator log, the
/// `tenant_msg` side is tenant-safe for the Kubernetes Event, `severity` tells the wrapper (via
/// [`finish_cleanup`]) whether to retry or treat the target as already gone, and `retry_outlook`
/// (meaningful only when `severity` is `Transient`) tells it whether that retry is expected to
/// help. Call sites keep the already-log-safe `Display` (backed by `kube_err_summary()` for the
/// `Kube` variant) for `error`, and route the same error through `TenantSafeMessage` for
/// `tenant_msg`.
#[derive(Debug)]
pub(super) struct CleanupFailure {
    error: anyhow::Error,
    tenant_msg: TenantSafeMessage,
    pub(super) severity: CleanupSeverity,
    retry_outlook: RetryOutlook,
}

/// Turns a single error into a [`CleanupFailure`].
///
/// `prefix` is a short, static description of the failing operation; the sanitizer's summary
/// of `e` is joined after it with `": "`. Callers must not include the separator in `prefix`.
trait CleanupMapErr<T> {
    type Error;
    fn cleanup_map_err<F>(self, prefix: &str, summary: F) -> Result<T, CleanupFailure>
    where
        F: FnOnce(&Self::Error) -> String;

    /// Like [`Self::cleanup_map_err`], but always classifies the failure as
    /// [`CleanupSeverity::Transient`], ignoring what [`ClassifyCleanupSeverity`] would otherwise
    /// say. Use this for LIST/collection calls (`Api::list`, `list_applications`,
    /// `list_sonarr`/`list_radarr`): a 404 there means the *endpoint* wasn't found (wrong route,
    /// CRD not yet served, misconfigured `urlBase`) — a real, retryable problem — not that the
    /// specific cleanup target is gone. [`ClassifyCleanupSeverity`] can't tell GET-by-id/DELETE
    /// apart from LIST from the error alone, so the call site must say which kind it is.
    fn cleanup_map_err_transient<F>(self, prefix: &str, summary: F) -> Result<T, CleanupFailure>
    where
        F: FnOnce(&Self::Error) -> String;
}

impl<T, E> CleanupMapErr<T> for Result<T, E>
where
    E: Into<TenantSafeMessage> + ClassifyCleanupSeverity,
{
    type Error = E;
    fn cleanup_map_err<F>(self, prefix: &str, summary: F) -> Result<T, CleanupFailure>
    where
        F: FnOnce(&E) -> String,
    {
        self.map_err(|e| {
            let severity = e.cleanup_severity();
            let retry_outlook = e.retry_outlook();
            CleanupFailure {
                error: anyhow::anyhow!("{prefix}: {}", summary(&e)),
                tenant_msg: e.into(),
                severity,
                retry_outlook,
            }
        })
    }

    fn cleanup_map_err_transient<F>(self, prefix: &str, summary: F) -> Result<T, CleanupFailure>
    where
        F: FnOnce(&E) -> String,
    {
        self.map_err(|e| {
            // Only `severity` is forced to `Transient` here (see the doc comment above) --
            // `retry_outlook` still reflects the real error, since a LIST call can fail on a
            // permission problem just as easily as a GET/DELETE can.
            let retry_outlook = e.retry_outlook();
            CleanupFailure {
                error: anyhow::anyhow!("{prefix}: {}", summary(&e)),
                tenant_msg: e.into(),
                severity: CleanupSeverity::Transient,
                retry_outlook,
            }
        })
    }
}

/// Map a curated-string failure (e.g. `AppDefaults::for_app`) into a [`CleanupFailure`]. The
/// anyhow message is `{ctx}: {e}`; the tenant-safe message is the curated string itself. Always
/// [`CleanupSeverity::Transient`] — a curated-string failure here means the app type's compiled
/// defaults didn't load, a config/programming problem unrelated to whether the downstream target
/// exists, so it must keep retrying rather than being folded into idempotent success. Always
/// [`RetryOutlook::NeedsManualFix`] too: a broken `image-defaults.toml` needs a redeploy, not a
/// passive retry.
fn cleanup_err_new(e: String, ctx: &str) -> CleanupFailure {
    CleanupFailure {
        error: anyhow::anyhow!("{ctx}: {e}"),
        tenant_msg: TenantSafeMessage::new(e),
        severity: CleanupSeverity::Transient,
        retry_outlook: RetryOutlook::NeedsManualFix,
    }
}

/// Inner cleanup body shared by [`cleanup_prowlarr_registration`] and the success-path tests.
///
/// `base_url_override` lets tests point the Prowlarr client at a MockServer instead of the
/// in-cluster `{name}.{ns}.svc` URL (which cannot resolve in tests); production passes `None`.
///
/// On failure returns a [`CleanupFailure`].
pub(super) async fn cleanup_prowlarr_registration_body(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    base_url_override: Option<&str>,
) -> Result<(), CleanupFailure> {
    use kube::api::ListParams;

    let app_name_str = servarr_resources::common::service_name(app);
    let defaults = servarr_crds::AppDefaults::try_for_app(&app.spec.app)
        .map_err(|e| cleanup_err_new(e, "failed to load app defaults"))?;
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let app_url = format!("http://{app_name_str}.{namespace}.svc:{port}");

    // Find the Prowlarr instance
    let sa_api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = sa_api
        .list(&ListParams::default())
        .await
        .cleanup_map_err_transient("failed to list ServarrApps", kube_err_summary)?;
    let prowlarr = apps.iter().find(|a| {
        a.spec.app == AppType::Prowlarr && a.spec.prowlarr_sync.as_ref().is_some_and(|s| s.enabled)
    });

    let Some(prowlarr) = prowlarr else {
        return Ok(()); // No Prowlarr with sync, nothing to clean up
    };

    let Some(secret_name) = prowlarr.spec.api_key_secret.as_deref() else {
        return Ok(());
    };

    let prowlarr_key = servarr_api::read_secret_key(client, namespace, secret_name, "api-key")
        .await
        .cleanup_map_err("failed to read Prowlarr API key", |e| e.log_summary())?;

    let prowlarr_app_name = servarr_resources::common::service_name(prowlarr);
    let prowlarr_defaults = servarr_crds::AppDefaults::try_for_app(&prowlarr.spec.app)
        .map_err(|e| cleanup_err_new(e, "failed to load app defaults"))?;
    let prowlarr_svc = prowlarr
        .spec
        .service
        .as_ref()
        .unwrap_or(&prowlarr_defaults.service);
    let prowlarr_port = prowlarr_svc.ports.first().map(|p| p.port).unwrap_or(80);
    let prowlarr_ns = prowlarr.namespace().unwrap_or_else(|| namespace.into());
    let prowlarr_url = base_url_override
        .map(str::to_owned)
        .unwrap_or_else(|| format!("http://{prowlarr_app_name}.{prowlarr_ns}.svc:{prowlarr_port}"));

    let prowlarr_client = servarr_api::ProwlarrClient::new(&prowlarr_url, &prowlarr_key)
        .cleanup_map_err("failed to create Prowlarr client", |e| e.log_summary())?;

    let existing = prowlarr_client
        .list_applications()
        .await
        .cleanup_map_err_transient("failed to list Prowlarr applications", |e| e.log_summary())?;
    if let Some(registered) = existing.iter().find(|a| {
        a.fields
            .iter()
            .any(|f| f.name == "baseUrl" && f.value.as_str() == Some(&app_url))
    }) {
        info!(
            app = %app.name_any(),
            prowlarr_app_id = registered.id,
            "removing app from Prowlarr on deletion"
        );
        prowlarr_client
            .delete_application(registered.id)
            .await
            .cleanup_map_err(
                &format!("failed to delete Prowlarr application {}", registered.id),
                |e| e.log_summary(),
            )?;

        publish_cleanup_normal(
            recorder,
            obj_ref,
            "ProwlarrCleanup",
            format!("Removed {} from Prowlarr", app.name_any()),
        )
        .await;
    }

    Ok(())
}

/// Remove this app's registration from Seerr when the CR is deleted.
///
/// See [`finish_cleanup`] for how the `Terminal`/`Transient` outcome of the cleanup body maps to
/// this function's return value and Event publication.
pub(crate) async fn cleanup_seerr_registration(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    base_url_override: Option<&str>,
) -> Result<(), anyhow::Error> {
    let outcome = cleanup_seerr_registration_body(
        client,
        app,
        namespace,
        recorder,
        obj_ref,
        base_url_override,
    )
    .await;
    finish_cleanup(outcome, "Seerr", app, recorder, obj_ref).await
}

/// Uniform view of the Seerr registered-server settings the cleanup path needs, so the
/// Sonarr/Radarr arms share one implementation.
pub(super) trait SeerrServerSettings {
    fn hostname(&self) -> &str;
    fn port(&self) -> f64;
    fn id(&self) -> i32;
}

impl SeerrServerSettings for overseerr::models::SonarrSettings {
    fn hostname(&self) -> &str {
        &self.hostname
    }
    fn port(&self) -> f64 {
        self.port
    }
    fn id(&self) -> i32 {
        self.id.unwrap_or(0.0) as i32
    }
}

impl SeerrServerSettings for overseerr::models::RadarrSettings {
    fn hostname(&self) -> &str {
        &self.hostname
    }
    fn port(&self) -> f64 {
        self.port
    }
    fn id(&self) -> i32 {
        self.id.unwrap_or(0.0) as i32
    }
}

/// Per-app Seerr operations (Sonarr/Radarr) so the cleanup helper is generic over the
/// registered-server settings type.
pub(super) trait SeerrAppKind {
    type Server: SeerrServerSettings;
    fn name(&self) -> &'static str;
    fn list<'a>(
        &'a self,
        client: &'a servarr_api::SeerrClient,
    ) -> futures::future::BoxFuture<'a, Result<Vec<Self::Server>, servarr_api::ApiError>>;
    fn delete<'a>(
        &'a self,
        client: &'a servarr_api::SeerrClient,
        id: i32,
    ) -> futures::future::BoxFuture<'a, Result<(), servarr_api::ApiError>>;
}

pub(super) struct SonarrSeerr;

impl SeerrAppKind for SonarrSeerr {
    type Server = overseerr::models::SonarrSettings;

    fn name(&self) -> &'static str {
        "Sonarr"
    }

    fn list<'a>(
        &'a self,
        client: &'a servarr_api::SeerrClient,
    ) -> futures::future::BoxFuture<'a, Result<Vec<Self::Server>, servarr_api::ApiError>> {
        Box::pin(client.list_sonarr())
    }

    fn delete<'a>(
        &'a self,
        client: &'a servarr_api::SeerrClient,
        id: i32,
    ) -> futures::future::BoxFuture<'a, Result<(), servarr_api::ApiError>> {
        Box::pin(client.delete_sonarr(id))
    }
}

struct RadarrSeerr;

impl SeerrAppKind for RadarrSeerr {
    type Server = overseerr::models::RadarrSettings;

    fn name(&self) -> &'static str {
        "Radarr"
    }

    fn list<'a>(
        &'a self,
        client: &'a servarr_api::SeerrClient,
    ) -> futures::future::BoxFuture<'a, Result<Vec<Self::Server>, servarr_api::ApiError>> {
        Box::pin(client.list_radarr())
    }

    fn delete<'a>(
        &'a self,
        client: &'a servarr_api::SeerrClient,
        id: i32,
    ) -> futures::future::BoxFuture<'a, Result<(), servarr_api::ApiError>> {
        Box::pin(client.delete_radarr(id))
    }
}

/// Turn a cleanup body's outcome into the wrapper's `Result<(), anyhow::Error>`, shared by
/// [`cleanup_prowlarr_registration`] and [`cleanup_seerr_registration`] (which differ only in
/// `cleanup_target` and which `_body` function produced `outcome`).
///
/// A [`CleanupSeverity::Terminal`] failure (downstream target provably absent) is treated as
/// idempotent success: logged, no `CleanupFailed` Event, `Ok(())` returned. A
/// [`CleanupSeverity::Transient`] failure publishes the `CleanupFailed` Event and returns the
/// full error so `reconcile()` keeps the finalizer and retries.
async fn finish_cleanup(
    outcome: Result<(), CleanupFailure>,
    cleanup_target: &str,
    app: &ServarrApp,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    match outcome {
        Ok(()) => Ok(()),
        Err(CleanupFailure {
            error,
            severity: CleanupSeverity::Terminal,
            ..
        }) => {
            info!(
                app = %app.name_any(),
                cleanup_target = %cleanup_target,
                error = %error,
                "cleanup target already absent, treating as complete"
            );
            Ok(())
        }
        Err(CleanupFailure {
            error,
            tenant_msg,
            severity: CleanupSeverity::Transient,
            retry_outlook,
        }) => {
            // #669 review (security-audit): retry_outlook stays operator-log-only, not on the
            // tenant-visible Event. kube_err_public_summary deliberately collapses every non-Api
            // kube::Error variant to one fixed string, because several of them (Auth,
            // Service/HyperError, InferConfig/InferKubeconfig, SerdeError) can carry sensitive
            // detail in their full Display -- Auth a bearer token, Service/HyperError an
            // internal endpoint, InferConfig/InferKubeconfig a kubeconfig path. Surfacing
            // NeedsManualFix vs MaySelfResolve on the Event would let a tenant with
            // get/list on events distinguish *which* collapsed variant fired (an operator
            // credential/config problem vs. a network blip) -- exactly the operator
            // control-plane detail that collapse exists to withhold. The full error (safe for
            // logs via `kube_err_summary`, already what `error` carries) plus the outlook are
            // fine on the operator-only log line below.
            warn!(
                app = %app.name_any(),
                cleanup_target = %cleanup_target,
                error = %error,
                retry_outlook = ?retry_outlook,
                "cleanup failed"
            );
            publish_cleanup_failed(recorder, obj_ref, &tenant_msg).await;
            Err(error)
        }
    }
}

/// Publish the Normal event for a successful finalizer cleanup (shared by the Prowlarr and
/// Seerr cleanup bodies).
async fn publish_cleanup_normal(
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    reason: &str,
    note: String,
) {
    super::publish_event(
        recorder,
        obj_ref,
        Event {
            type_: EventType::Normal,
            reason: reason.into(),
            note: Some(note),
            action: "Finalize".into(),
            secondary: None,
        },
    )
    .await;
}

/// Publish the Warning event (reason `CleanupFailed`) for a failed finalizer cleanup, shared
/// by the Prowlarr and Seerr cleanup wrappers. The note carries the tenant-safe message
/// from the propagated cleanup error. Deliberately doesn't vary by `RetryOutlook` -- see the
/// comment at the `finish_cleanup` call site for why that distinction stays operator-log-only.
async fn publish_cleanup_failed(
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    msg: &TenantSafeMessage,
) {
    // A failure to publish means the tenant never learns cleanup failed. publish_event warns
    // and counts the failure metric, so the loss is visible in the operator log and in
    // metrics rather than swallowed. (#708)
    super::publish_event(
        recorder,
        obj_ref,
        Event {
            type_: EventType::Warning,
            reason: "CleanupFailed".into(),
            note: Some(msg.as_ref().to_string()),
            action: "Finalize".into(),
            secondary: None,
        },
    )
    .await;
}

/// Remove one registered server (Sonarr or Radarr) matching `app_hostname:port` from Seerr.
/// Returns `true` when a server was removed so the caller publishes the Normal event.
pub(super) async fn seerr_remove_server<K>(
    seerr_client: &servarr_api::SeerrClient,
    app: &ServarrApp,
    app_hostname: &str,
    port: i32,
    kind: K,
) -> Result<bool, CleanupFailure>
where
    K: SeerrAppKind,
{
    let existing = kind.list(seerr_client).await.cleanup_map_err_transient(
        &format!("failed to list Seerr {} servers", kind.name()),
        |e| e.log_summary(),
    )?;
    if let Some(registered) = existing
        .iter()
        .find(|s| s.hostname() == app_hostname && s.port() == f64::from(port))
    {
        let id = registered.id();
        info!(
            app = %app.name_any(),
            seerr_server_id = id,
            "removing {} from Seerr on deletion",
            kind.name()
        );
        kind.delete(seerr_client, id).await.cleanup_map_err(
            &format!("failed to delete Seerr {} server {id}", kind.name()),
            |e| e.log_summary(),
        )?;
        return Ok(true);
    }
    Ok(false)
}

/// Inner cleanup body shared by [`cleanup_seerr_registration`] and the success-path tests.
///
/// `base_url_override` lets tests point the Seerr client at a MockServer instead of the
/// in-cluster `{name}.{ns}.svc` URL (which cannot resolve in tests); production passes `None`.
///
/// On failure returns a [`CleanupFailure`].
pub(super) async fn cleanup_seerr_registration_body(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    base_url_override: Option<&str>,
) -> Result<(), CleanupFailure> {
    use kube::api::ListParams;

    let app_name_str = servarr_resources::common::service_name(app);
    let defaults_for_app = servarr_crds::AppDefaults::try_for_app(&app.spec.app)
        .map_err(|e| cleanup_err_new(e, "failed to load app defaults"))?;
    let svc_spec = app
        .spec
        .service
        .as_ref()
        .unwrap_or(&defaults_for_app.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let app_hostname = format!("{app_name_str}.{namespace}.svc");

    // Find the Seerr instance
    let sa_api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = sa_api
        .list(&ListParams::default())
        .await
        .cleanup_map_err_transient("failed to list ServarrApps", kube_err_summary)?;
    let seerr = apps.iter().find(|a| {
        a.spec.app == AppType::Seerr && a.spec.seerr_sync.as_ref().is_some_and(|s| s.enabled)
    });

    let Some(seerr) = seerr else {
        return Ok(());
    };

    let Some(secret_name) = seerr.spec.api_key_secret.as_deref() else {
        return Ok(());
    };

    let seerr_ns = seerr.namespace().unwrap_or_else(|| namespace.into());
    let seerr_key = servarr_api::read_secret_key(client, &seerr_ns, secret_name, "api-key")
        .await
        .cleanup_map_err("failed to read Seerr API key", |e| e.log_summary())?;

    let seerr_app_name = servarr_resources::common::service_name(seerr);
    let seerr_defaults = servarr_crds::AppDefaults::try_for_app(&seerr.spec.app)
        .map_err(|e| cleanup_err_new(e, "failed to load app defaults"))?;
    let seerr_svc = seerr
        .spec
        .service
        .as_ref()
        .unwrap_or(&seerr_defaults.service);
    let seerr_port = seerr_svc.ports.first().map(|p| p.port).unwrap_or(80);
    let seerr_url = base_url_override
        .map(str::to_owned)
        .unwrap_or_else(|| format!("http://{seerr_app_name}.{seerr_ns}.svc:{seerr_port}"));

    let seerr_client = servarr_api::SeerrClient::new(&seerr_url, &seerr_key);

    // Remove matching Sonarr or Radarr server by hostname + port
    let removed = match app.spec.app {
        AppType::Sonarr => {
            seerr_remove_server(&seerr_client, app, &app_hostname, port, SonarrSeerr).await?
        }
        AppType::Radarr => {
            seerr_remove_server(&seerr_client, app, &app_hostname, port, RadarrSeerr).await?
        }
        _ => false,
    };
    if removed {
        publish_cleanup_normal(
            recorder,
            obj_ref,
            "SeerrCleanup",
            format!("Removed {} from Seerr", app.name_any()),
        )
        .await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::MockServer;

    fn make_recorder(client: &Client) -> Recorder {
        Recorder::new(
            client.clone(),
            kube::runtime::events::Reporter {
                controller: "servarr-operator".into(),
                instance: None,
            },
        )
    }

    fn make_obj_ref() -> k8s_openapi::api::core::v1::ObjectReference {
        k8s_openapi::api::core::v1::ObjectReference {
            kind: Some("ServarrApp".into()),
            name: Some("my-app".into()),
            namespace: Some("test".into()),
            uid: Some("app-uid-1".into()),
            ..Default::default()
        }
    }

    // ---- #708: both cleanup publish sites route through the shared publish_event helper,
    // so an Events-API failure counts the same metric the controller.rs sites already count.

    #[tokio::test]
    async fn publish_cleanup_normal_increments_failure_metric_on_publish_failure() {
        // No Mock is mounted for the events endpoint, so wiremock answers 404 and the
        // publish fails.
        let mock_server = MockServer::start().await;
        let client = crate::testutils::build_mock_client(&mock_server.uri()).await;
        let recorder = make_recorder(&client);

        let before = crate::metrics::EVENT_PUBLISH_FAILURES_TOTAL
            .with_label_values(&["CleanupNormalMetricTest"])
            .get();

        publish_cleanup_normal(
            &recorder,
            &make_obj_ref(),
            "CleanupNormalMetricTest",
            "test note".to_string(),
        )
        .await;

        let after = crate::metrics::EVENT_PUBLISH_FAILURES_TOTAL
            .with_label_values(&["CleanupNormalMetricTest"])
            .get();
        assert_eq!(after, before + 1);
    }

    #[tokio::test]
    async fn publish_cleanup_failed_increments_failure_metric_on_publish_failure() {
        let mock_server = MockServer::start().await;
        let client = crate::testutils::build_mock_client(&mock_server.uri()).await;
        let recorder = make_recorder(&client);

        let before = crate::metrics::EVENT_PUBLISH_FAILURES_TOTAL
            .with_label_values(&["CleanupFailed"])
            .get();

        publish_cleanup_failed(
            &recorder,
            &make_obj_ref(),
            &TenantSafeMessage::new("cleanup went wrong"),
        )
        .await;

        let after = crate::metrics::EVENT_PUBLISH_FAILURES_TOTAL
            .with_label_values(&["CleanupFailed"])
            .get();
        assert_eq!(after, before + 1);
    }
}
