use kube::api::Api;
use kube::runtime::events::{Event, EventType, Recorder};
use kube::{Client, ResourceExt};
use servarr_api::TenantSafeMessage;
use servarr_api::k8s::{is_kube_not_found, kube_err_summary};
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

/// Classifies an error's [`CleanupSeverity`]. Implemented only for the concrete error types the
/// cleanup path actually produces (`kube::Error`, `SecretError`, `ApiError`) — deliberately no
/// blanket/default impl, so a new error type flowing through [`CleanupMapErr`] must get an
/// explicit, reviewed classification rather than silently defaulting to one severity or the other.
pub(super) trait ClassifyCleanupSeverity {
    fn cleanup_severity(&self) -> CleanupSeverity;
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
}

/// Remove this app's registration from Prowlarr when the CR is deleted.
///
/// See [`finish_cleanup`] for how the `Terminal`/`Transient` outcome of the cleanup body maps to
/// this function's return value and Event publication.
pub(super) async fn cleanup_prowlarr_registration(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    let outcome =
        cleanup_prowlarr_registration_body(client, app, namespace, recorder, obj_ref, None).await;
    finish_cleanup(outcome, "Prowlarr", app, recorder, obj_ref).await
}

/// A cleanup body's failure: the `error` side carries full detail for the operator log, the
/// `tenant_msg` side is tenant-safe for the Kubernetes Event, and `severity` tells the wrapper
/// (via [`finish_cleanup`]) whether to retry or treat the target as already gone. Call sites keep
/// the already-log-safe `Display` (backed by `kube_err_summary()` for the `Kube` variant) for
/// `error`, and route the same error through `TenantSafeMessage` for `tenant_msg`.
#[derive(Debug)]
pub(super) struct CleanupFailure {
    error: anyhow::Error,
    tenant_msg: TenantSafeMessage,
    pub(super) severity: CleanupSeverity,
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
            CleanupFailure {
                error: anyhow::anyhow!("{prefix}: {}", summary(&e)),
                tenant_msg: e.into(),
                severity,
            }
        })
    }

    fn cleanup_map_err_transient<F>(self, prefix: &str, summary: F) -> Result<T, CleanupFailure>
    where
        F: FnOnce(&E) -> String,
    {
        self.map_err(|e| CleanupFailure {
            error: anyhow::anyhow!("{prefix}: {}", summary(&e)),
            tenant_msg: e.into(),
            severity: CleanupSeverity::Transient,
        })
    }
}

/// Map a curated-string failure (e.g. `AppDefaults::for_app`) into a [`CleanupFailure`]. The
/// anyhow message is `{ctx}: {e}`; the tenant-safe message is the curated string itself. Always
/// [`CleanupSeverity::Transient`] — a curated-string failure here means the app type's compiled
/// defaults didn't load, a config/programming problem unrelated to whether the downstream target
/// exists, so it must keep retrying rather than being folded into idempotent success.
fn cleanup_err_new(e: String, ctx: &str) -> CleanupFailure {
    CleanupFailure {
        error: anyhow::anyhow!("{ctx}: {e}"),
        tenant_msg: TenantSafeMessage::new(e),
        severity: CleanupSeverity::Transient,
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
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app)
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
    let prowlarr_defaults = servarr_crds::AppDefaults::for_app(&prowlarr.spec.app)
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
pub(super) async fn cleanup_seerr_registration(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    let outcome =
        cleanup_seerr_registration_body(client, app, namespace, recorder, obj_ref, None).await;
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
        }) => {
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
    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: reason.into(),
                note: Some(note),
                action: "Finalize".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;
}

/// Publish the Warning event (reason `CleanupFailed`) for a failed finalizer cleanup, shared
/// by the Prowlarr and Seerr cleanup wrappers. The note carries the tenant-safe message
/// from the propagated cleanup error.
async fn publish_cleanup_failed(
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    msg: &TenantSafeMessage,
) {
    if let Err(e) = recorder
        .publish(
            &Event {
                type_: EventType::Warning,
                reason: "CleanupFailed".into(),
                note: Some(msg.as_ref().to_string()),
                action: "Finalize".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await
    {
        // A failure to publish means the tenant never learns cleanup failed; log it rather
        // than swallowing it silently. The full error is operator-log-only (never tenant-facing).
        warn!(
            error = %e,
            object = %obj_ref.name.as_deref().unwrap_or("<unknown>"),
            "failed to publish CleanupFailed event"
        );
    }
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
    let defaults_for_app = servarr_crds::AppDefaults::for_app(&app.spec.app)
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
    let seerr_defaults = servarr_crds::AppDefaults::for_app(&seerr.spec.app)
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
