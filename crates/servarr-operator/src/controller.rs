use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, PersistentVolumeClaim, Secret, Service};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event, EventType, Recorder};
use kube::runtime::reflector::{self, ObjectRef};
use kube::runtime::watcher;
use kube::{Client, CustomResourceExt, Resource, ResourceExt};
use servarr_api::AppKind;
use servarr_api::TenantSafeMessage;
use servarr_api::k8s::{kube_err_public_summary, kube_err_summary};
use servarr_crds::{AppType, Condition, ServarrApp, ServarrAppStatus, condition_types};
use thiserror::Error;
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use crate::context::Context;
use crate::metrics::{
    increment_backup_operations, increment_drift_corrections, increment_reconcile_total,
    observe_reconcile_duration, set_managed_apps,
};

fn app_type_to_kind(app_type: &AppType) -> Option<AppKind> {
    match app_type {
        AppType::Sonarr => Some(AppKind::Sonarr),
        AppType::Radarr => Some(AppKind::Radarr),
        AppType::Lidarr => Some(AppKind::Lidarr),
        AppType::Prowlarr => Some(AppKind::Prowlarr),
        _ => None,
    }
}

const FIELD_MANAGER: &str = "servarr-operator";

// Prowlarr/Seerr cleanup finalizers for Servarr v3 apps (Sonarr/Radarr/Lidarr). Module-level
// so both `reconcile()` and its tests reference the same source of truth.
const PROWLARR_FINALIZER: &str = "servarr.dev/prowlarr-sync";
// Issue #44: value intentionally unchanged from "servarr.dev/overseerr-sync" — this string is
// already attached to existing ServarrApp objects' metadata.finalizers. Changing it would orphan
// the finalizer on any CR that has it: nothing would ever match the old string to remove it, and
// the object would hang in Terminating forever on delete.
const SEERR_FINALIZER: &str = "servarr.dev/overseerr-sync";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Kubernetes API error: {0}")]
    Kube(#[source] kube::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
    #[error("app defaults error: {0}")]
    AppDefaults(String),
    #[error("finalizer cleanup pending retry")]
    CleanupPending,
}

impl Error {
    /// Returns a log-safe summary. The `Kube` variant delegates to [`kube_err_summary`]; the
    /// other variants already only carry curated messages, never raw external response content.
    pub fn log_summary(&self) -> String {
        match self {
            Self::Kube(e) => kube_err_summary(e),
            other => other.to_string(),
        }
    }

    /// Returns a tenant-safe summary. The `Kube` variant delegates to
    /// [`kube_err_public_summary`]; the other variants are safe as-is (see [`Self::log_summary`]).
    pub fn public_summary(&self) -> String {
        match self {
            Self::Kube(e) => kube_err_public_summary(e),
            other => other.to_string(),
        }
    }
}

pub fn print_crd() -> Result<()> {
    let crd = ServarrApp::crd();
    let yaml = serde_yaml::to_string(&crd)?;
    println!("{yaml}");
    Ok(())
}

pub async fn run(client: kube::Client, server_state: crate::server::ServerState) -> Result<()> {
    // Validate that every AppType has a complete entry in image-defaults.toml.
    // Fail fast at startup rather than panicking inside the reconcile hot path.
    servarr_crds::AppDefaults::validate_all()
        .map_err(|e| anyhow::anyhow!("image-defaults.toml validation failed: {e}"))?;

    let ctx = Arc::new(Context::new(client.clone()));

    let (apps, deployments, services, config_maps, secrets) =
        if let Some(ref ns) = ctx.watch_namespace {
            (
                Api::<ServarrApp>::namespaced(client.clone(), ns),
                Api::<Deployment>::namespaced(client.clone(), ns),
                Api::<Service>::namespaced(client.clone(), ns),
                Api::<ConfigMap>::namespaced(client.clone(), ns),
                Api::<Secret>::namespaced(client.clone(), ns),
            )
        } else {
            (
                Api::<ServarrApp>::all(client.clone()),
                Api::<Deployment>::all(client.clone()),
                Api::<Service>::all(client.clone()),
                Api::<ConfigMap>::all(client.clone()),
                Api::<Secret>::all(client.clone()),
            )
        };

    // Build a reflector store so the secret watcher mapper can look up which
    // ServarrApps reference a changed secret without an async API call.
    let (app_store, app_writer) = reflector::store::<ServarrApp>();
    let app_store_for_watcher = app_store.clone();

    // Background task: keep the store up-to-date by watching ServarrApps.
    // This runs independently of the Controller's own internal watcher.
    let apps_for_reflector = if let Some(ref ns) = ctx.watch_namespace {
        Api::<ServarrApp>::namespaced(client.clone(), ns)
    } else {
        Api::<ServarrApp>::all(client.clone())
    };
    tokio::spawn(async move {
        reflector::reflector(
            app_writer,
            watcher::watcher(apps_for_reflector, watcher::Config::default()),
        )
        .for_each(|_| std::future::ready(()))
        .await;
    });

    info!("Starting Servarr Operator controller");
    server_state.set_ready();

    Controller::new(apps, watcher::Config::default())
        .owns(deployments, watcher::Config::default())
        .owns(services, watcher::Config::default())
        .owns(config_maps, watcher::Config::default())
        // Watch admin-credential secrets: when a secret changes, enqueue all
        // ServarrApps that reference it so credential rotation propagates immediately.
        .watches(secrets, watcher::Config::default(), move |secret| {
            let secret_name = secret.name_any();
            app_store_for_watcher
                .state()
                .into_iter()
                .filter(move |app| {
                    app.spec
                        .admin_credentials
                        .as_ref()
                        .is_some_and(|ac| ac.secret_name == secret_name)
                })
                .map(|app| ObjectRef::from_obj(&*app))
                .collect::<Vec<_>>()
        })
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled"),
                Err(e) => error!(%e, "reconcile error"),
            }
        })
        .await;

    Ok(())
}

pub async fn reconcile(app: Arc<ServarrApp>, ctx: Arc<Context>) -> Result<Action, Error> {
    let client = &ctx.client;
    let name = app.name_any();
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let pp = PatchParams::apply(FIELD_MANAGER).force();

    let recorder = Recorder::new(client.clone(), ctx.reporter.clone());
    let obj_ref = app.object_ref(&());

    info!(%name, %ns, app_type = %app.spec.app, "reconciling");

    let app_type = app.spec.app.as_str();
    let start_time = std::time::Instant::now();

    // Prowlarr cleanup finalizer for Servarr v3 apps
    if matches!(
        app.spec.app,
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr
    ) {
        if app.metadata.deletion_timestamp.is_some() {
            // App is being deleted — clean up Prowlarr registration
            let prowlarr_result =
                cleanup_prowlarr_registration(client, &app, &ns, &recorder, &obj_ref).await;
            if let Err(ref e) = prowlarr_result {
                warn!(%name, error = %e, "failed to clean up Prowlarr registration");
            }
            // App is being deleted — clean up Seerr registration
            let seerr_result =
                cleanup_seerr_registration(client, &app, &ns, &recorder, &obj_ref).await;
            if let Err(ref e) = seerr_result {
                warn!(%name, error = %e, "failed to clean up Seerr registration");
            }

            // Drop a finalizer only once its cleanup has actually completed — succeeded
            // outright, or proved the downstream target already gone (see the terminal
            // handling in `cleanup_prowlarr_registration`/`cleanup_seerr_registration`,
            // which folds that case into `Ok`). A transient failure keeps its finalizer so
            // the cleanup it gates is retried on the next reconcile instead of being
            // silently dropped.
            let existing_finalizers = app.metadata.finalizers.clone().unwrap_or_default();
            let finalizers: Vec<String> = existing_finalizers
                .iter()
                .filter(|x| {
                    !(prowlarr_result.is_ok() && *x == PROWLARR_FINALIZER
                        || seerr_result.is_ok() && *x == SEERR_FINALIZER)
                })
                .cloned()
                .collect();

            if finalizers != existing_finalizers {
                let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
                let patch = serde_json::json!({
                    "metadata": { "finalizers": finalizers }
                });
                sa_api
                    .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                    .await
                    .map_err(Error::Kube)?;
            }

            // A cleanup finalizer still present after the filter means its cleanup is still
            // pending (it transiently failed) — surface an error so `error_policy` requeues,
            // instead of `Ok(Action::await_change())`, which would otherwise wait indefinitely
            // for an unrelated watch event. A finalizer that was never present to begin with
            // (e.g. no Prowlarr/Seerr sync was ever configured for this namespace) never
            // blocks progress here, even if its no-op cleanup attempt happened to hit a
            // transient error of its own (e.g. the ServarrApps list call failing).
            let still_pending = finalizers
                .iter()
                .any(|x| x == PROWLARR_FINALIZER || x == SEERR_FINALIZER);
            if still_pending {
                return Err(Error::CleanupPending);
            }
            return Ok(Action::await_change());
        }

        // Ensure finalizer is present if a Prowlarr with sync enabled exists
        let has_prowlarr_finalizer = app
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|f| f.contains(&PROWLARR_FINALIZER.to_string()));
        if !has_prowlarr_finalizer && prowlarr_sync_exists(client, &ns).await {
            let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
            let mut finalizers = app.metadata.finalizers.clone().unwrap_or_default();
            finalizers.push(PROWLARR_FINALIZER.to_string());
            let patch = serde_json::json!({
                "metadata": { "finalizers": finalizers }
            });
            sa_api
                .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                .await
                .map_err(Error::Kube)?;
        }

        // Ensure Seerr finalizer is present if an Seerr with sync enabled exists
        let has_seerr_finalizer = app
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|f| f.contains(&SEERR_FINALIZER.to_string()));
        if !has_seerr_finalizer && seerr_sync_exists(client, &ns).await {
            let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
            let mut finalizers = app.metadata.finalizers.clone().unwrap_or_default();
            finalizers.push(SEERR_FINALIZER.to_string());
            let patch = serde_json::json!({
                "metadata": { "finalizers": finalizers }
            });
            sa_api
                .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                .await
                .map_err(Error::Kube)?;
        }
    }

    // Check for restore-from-backup annotation
    let restore_condition = if let Some(restore_id) = app
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("servarr.dev/restore-from"))
        .cloned()
    {
        let now = chrono_now();
        let result = maybe_restore_backup(client, &app, &restore_id, &recorder, &obj_ref).await;
        Some(result_to_condition(
            result,
            ConditionSpec {
                condition_type: condition_types::RESTORE_READY,
                ok_reason: "RestoreComplete",
                ok_message: &format!("Restored from backup {restore_id}"),
                fail_reason: "RestoreFailed",
                fail_log: "restore-from-backup failed",
            },
            &name,
            &now,
        ))
    } else {
        None
    };

    // Build and apply Deployment
    let deployment = servarr_resources::deployment::build(&app, &ctx.image_overrides)
        .map_err(Error::AppDefaults)?;
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), &ns);

    // Issue #44: an AppType rename (e.g. Overseerr -> Seerr) changes the
    // `app.kubernetes.io/name` selector label baked into
    // Deployment.spec.selector.matchLabels. That field is immutable on
    // apps/v1, so SSA-patching an existing Deployment whose live selector no
    // longer matches the desired one is rejected by the API server, wedging
    // reconciliation forever. Detect that drift and delete the stale
    // Deployment first so the patch below recreates it fresh. PVCs are owned
    // by the ServarrApp CR, not the Deployment (see servarr_resources::pvc),
    // so deleting the Deployment never touches persisted data.
    match deploy_api.get(&name).await {
        Err(kube::Error::Api(err)) if err.code == 404 => {}
        Err(e) => return Err(Error::Kube(e)),
        Ok(existing) => {
            // Only ever delete a Deployment this ServarrApp owns. A foreign
            // object with a colliding name must not be torn down by our
            // reconcile; the SSA patch below will surface any real conflict.
            let owned_by_app = existing
                .metadata
                .owner_references
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|r| Some(r.uid.as_str()) == app.metadata.uid.as_deref());
            if owned_by_app {
                let live_selector = existing
                    .spec
                    .as_ref()
                    .and_then(|s| s.selector.match_labels.as_ref());
                let desired_selector = deployment
                    .spec
                    .as_ref()
                    .and_then(|s| s.selector.match_labels.as_ref());
                if live_selector != desired_selector {
                    warn!(
                        %name,
                        "Deployment selector changed (immutable field) — deleting to recreate"
                    );
                    deploy_api
                        .delete(&name, &DeleteParams::default())
                        .await
                        .map_err(Error::Kube)?;
                }
            }
        }
    }

    tracing::debug!(%name, "SSA: applying Deployment");
    deploy_api
        .patch(&name, &pp, &Patch::Apply(&deployment))
        .await
        .map_err(Error::Kube)?;

    // Check for drift: read back the Deployment and compare only operator-managed fields.
    // Kubernetes adds default fields (terminationGracePeriodSeconds, dnsPolicy, etc.)
    // so we check that our desired fields are a subset of the actual state.
    tracing::debug!(%name, "getting Deployment for drift check");
    let applied_deploy = deploy_api.get(&name).await.map_err(Error::Kube)?;
    if let (Some(desired_spec), Some(actual_spec)) =
        (deployment.spec.as_ref(), applied_deploy.spec.as_ref())
    {
        let desired_json = match serde_json::to_value(&desired_spec.template) {
            Ok(v) => v,
            Err(e) => {
                warn!(%name, error = %e, "drift check: failed to serialize desired template, skipping");
                return Ok(Action::requeue(Duration::from_secs(300)));
            }
        };
        let actual_json = match serde_json::to_value(&actual_spec.template) {
            Ok(v) => v,
            Err(e) => {
                warn!(%name, error = %e, "drift check: failed to serialize actual template, skipping");
                return Ok(Action::requeue(Duration::from_secs(300)));
            }
        };
        if !json_is_subset(&desired_json, &actual_json) {
            let diff = json_diff_paths(&desired_json, &actual_json, "".to_string());
            warn!(%name, "deployment drift detected, re-applying");
            tracing::debug!(%name, ?diff, "drift details");
            recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "DriftDetected".into(),
                        note: Some("Deployment pod template differs from desired state".into()),
                        action: "DriftCheck".into(),
                        secondary: None,
                    },
                    &obj_ref,
                )
                .await
                .map_err(Error::Kube)?;
            increment_drift_corrections(app_type, &ns, "Deployment");
            // Re-apply to correct drift
            tracing::debug!(%name, "SSA: re-applying Deployment (drift correction)");
            deploy_api
                .patch(&name, &pp, &Patch::Apply(&deployment))
                .await
                .map_err(Error::Kube)?;
        }
    }

    // Build and apply Service. The Service name may differ from the app name
    // when `service_name` is set, so the SSA URL must use the service name.
    let service = servarr_resources::service::build(&app).map_err(Error::AppDefaults)?;
    let svc_name = servarr_resources::common::service_name(&app);
    let svc_api = Api::<Service>::namespaced(client.clone(), &ns);
    tracing::debug!(%svc_name, "SSA: applying Service");
    svc_api
        .patch(&svc_name, &pp, &Patch::Apply(&service))
        .await
        .map_err(Error::Kube)?;

    // Build and apply PVCs (get-or-create to avoid mutating immutable fields)
    let pvcs = servarr_resources::pvc::build_all(&app).map_err(Error::AppDefaults)?;
    let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), &ns);
    for pvc in &pvcs {
        let pvc_name = pvc.metadata.name.as_deref().unwrap_or("unknown");
        match pvc_api.get(pvc_name).await {
            Ok(_) => {
                // PVC exists, don't modify (immutable fields)
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                pvc_api
                    .patch(pvc_name, &pp, &Patch::Apply(pvc))
                    .await
                    .map_err(Error::Kube)?;
            }
            Err(e) => return Err(Error::Kube(e)),
        }
    }

    // Build and apply NetworkPolicy.
    // Enabled when: network_policy_config is set (takes precedence), or the
    // boolean network_policy flag is true (default).
    let has_explicit_config = app.spec.network_policy_config.is_some();
    let network_policy_enabled = has_explicit_config || app.spec.network_policy.unwrap_or(true);
    if has_explicit_config && app.spec.network_policy == Some(false) {
        tracing::debug!(
            app = %name,
            "network_policy_config is set; overriding network_policy=false"
        );
    }
    if network_policy_enabled {
        let np = servarr_resources::networkpolicy::build(&app).map_err(Error::AppDefaults)?;
        let np_api = Api::<NetworkPolicy>::namespaced(client.clone(), &ns);
        tracing::debug!(%name, "SSA: applying NetworkPolicy");
        np_api
            .patch(&name, &pp, &Patch::Apply(&np))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply ConfigMap (Transmission settings, SABnzbd whitelist)
    if let Some(cm) = servarr_resources::configmap::build(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        tracing::debug!(%name, cm_name, "SSA: applying ConfigMap");
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply tar-unpack ConfigMap (SABnzbd)
    if let Some(cm) = servarr_resources::configmap::build_tar_unpack(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        tracing::debug!(%name, cm_name, "SSA: applying tar-unpack ConfigMap");
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply Prowlarr custom definitions ConfigMap
    if let Some(cm) = servarr_resources::configmap::build_prowlarr_definitions(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        tracing::debug!(%name, cm_name, "SSA: applying Prowlarr definitions ConfigMap");
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply Bazarr init ConfigMap (pre-seeds config.yaml before first boot)
    if let Some(cm) = servarr_resources::configmap::build_bazarr_init(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        tracing::debug!(%name, cm_name, "SSA: applying Bazarr init ConfigMap");
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Auto-create API key Secret if apiKeySecret is set and the Secret is absent.
    // Uses a get-then-create pattern so an existing key is never overwritten.
    tracing::debug!(%name, "ensuring API key secret");
    ensure_api_key_secret(client, &app).await?;

    // For Servarr v3 apps (Sonarr/Radarr/Lidarr/Prowlarr) credentials are applied
    // via PUT /api/v3/config/host after each pod start (sync_admin_credentials).
    // Patch a checksum annotation on the pod template so Kubernetes rolls pods
    // when the Secret rotates, giving sync_admin_credentials a fresh target.
    //
    // Transmission MUST NOT get a checksum annotation: the LSIO init script
    // rewrites settings.json on every container start, so a rolling update would
    // race and reset auth to false before the next reconcile can re-apply it.
    let needs_rollout_on_secret_change = matches!(
        app.spec.app,
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr
    );
    if needs_rollout_on_secret_change && let Some(ref ac) = app.spec.admin_credentials {
        tracing::debug!(%name, secret_name = %ac.secret_name, "patching admin credentials checksum");
        patch_admin_credentials_checksum(client, &app, &ac.secret_name).await?;
    }

    // Build and apply SSH bastion authorized-keys Secret
    if let Some(secret) = servarr_resources::secret::build_authorized_keys(&app) {
        let secret_name = secret.metadata.name.as_deref().unwrap_or(&name);
        let secret_api = Api::<Secret>::namespaced(client.clone(), &ns);
        tracing::debug!(%name, secret_name, "SSA: applying SSH bastion authorized-keys Secret");
        secret_api
            .patch(secret_name, &pp, &Patch::Apply(&secret))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply SSH bastion restricted-rsync ConfigMap
    if let Some(cm) = servarr_resources::configmap::build_ssh_bastion_restricted_rsync(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        tracing::debug!(%name, cm_name, "SSA: applying SSH bastion restricted-rsync ConfigMap");
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply HTTPRoute or TCPRoute (if gateway enabled)
    // Gateway API types use DynamicObject since they're not in k8s-openapi
    if let Some(route) = servarr_resources::tcproute::build(&app).map_err(Error::AppDefaults)? {
        // TCPRoute takes precedence when route_type is Tcp or TLS is enabled
        let api_resource = kube::discovery::ApiResource {
            group: "gateway.networking.k8s.io".into(),
            version: "v1alpha2".into(),
            api_version: "gateway.networking.k8s.io/v1alpha2".into(),
            kind: "TCPRoute".into(),
            plural: "tcproutes".into(),
        };
        let route_api =
            Api::<kube::api::DynamicObject>::namespaced_with(client.clone(), &ns, &api_resource);
        let route_data = serde_json::to_value(&route).map_err(Error::Serialization)?;
        tracing::debug!(%name, "SSA: applying TCPRoute");
        route_api
            .patch(&name, &pp, &Patch::Apply(route_data))
            .await
            .map_err(Error::Kube)?;
    } else if let Some(route) =
        servarr_resources::httproute::build(&app).map_err(Error::AppDefaults)?
    {
        let api_resource = kube::discovery::ApiResource {
            group: "gateway.networking.k8s.io".into(),
            version: "v1".into(),
            api_version: "gateway.networking.k8s.io/v1".into(),
            kind: "HTTPRoute".into(),
            plural: "httproutes".into(),
        };
        let route_api =
            Api::<kube::api::DynamicObject>::namespaced_with(client.clone(), &ns, &api_resource);
        let route_data = serde_json::to_value(&route).map_err(Error::Serialization)?;
        tracing::debug!(%name, "SSA: applying HTTPRoute");
        route_api
            .patch(&name, &pp, &Patch::Apply(route_data))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply cert-manager Certificate (if TLS is enabled)
    if let Some(cert) = servarr_resources::certificate::build(&app).map_err(Error::AppDefaults)? {
        let api_resource = kube::discovery::ApiResource {
            group: "cert-manager.io".into(),
            version: "v1".into(),
            api_version: "cert-manager.io/v1".into(),
            kind: "Certificate".into(),
            plural: "certificates".into(),
        };
        let cert_api =
            Api::<kube::api::DynamicObject>::namespaced_with(client.clone(), &ns, &api_resource);
        let cert_data = serde_json::to_value(&cert).map_err(Error::Serialization)?;
        tracing::debug!(%name, "SSA: applying Certificate");
        cert_api
            .patch(&name, &pp, &Patch::Apply(cert_data))
            .await
            .map_err(Error::Kube)?;
    }

    // Resolve the Transmission client once per reconcile (base URL + adminCredentials secret +
    // TransmissionClient) and share it between check_api_health and check_download_client_health
    // -- both gate on the same apiHealthCheck.enabled flag, so building it twice means double
    // the Secret GETs and a redundant session-ID handshake against the same app (#499).
    let transmission_access: Option<Result<TransmissionAccess, String>> =
        if transmission_health_check_enabled(&app) {
            Some(match resolve_service_base_url(&app, &ns) {
                Ok(base_url) => resolve_transmission_access(client, &app, &ns, &base_url).await,
                Err(e) => Err(e),
            })
        } else {
            None
        };

    // API health check and update check (non-blocking)
    let (health_condition, update_condition) =
        check_api_health(client, &app, transmission_access.as_ref()).await;

    // Admin credential sync via live API (SABnzbd, Transmission, Jellyfin, Tautulli, Seerr)
    let admin_creds_condition =
        sync_admin_credentials(client, &app, transmission_access.as_ref()).await;
    // If sync failed (app not ready yet), requeue sooner than the default 300s so
    // credentials are applied once the app becomes healthy.
    let admin_creds_pending = admin_creds_condition
        .as_ref()
        .map(|c| c.status != "True")
        .unwrap_or(false);

    // Backup scheduling (non-blocking)
    let backup_status = maybe_run_backup(client, &app, &recorder, &obj_ref).await;

    // Prowlarr cross-app sync (only for Prowlarr-type apps with sync enabled)
    let prowlarr_sync_condition = if app.spec.app == AppType::Prowlarr
        && let Some(ref sync_spec) = app.spec.prowlarr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        let now = chrono_now();
        let result = sync_prowlarr_apps(client, &app, target_ns, &recorder, &obj_ref).await;
        Some(result_to_condition(
            result,
            ConditionSpec {
                condition_type: condition_types::PROWLARR_SYNC_READY,
                ok_reason: "SyncComplete",
                ok_message: "Sonarr, Radarr, and Lidarr synced from Prowlarr",
                fail_reason: "SyncFailed",
                fail_log: "Prowlarr sync failed",
            },
            &name,
            &now,
        ))
    } else {
        None
    };

    // Seerr cross-app sync (only for Seerr-type apps with sync enabled)
    let seerr_sync_condition = if app.spec.app == AppType::Seerr
        && let Some(ref sync_spec) = app.spec.seerr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        let now = chrono_now();
        let result = sync_seerr_servers(client, &app, target_ns, &recorder, &obj_ref).await;
        Some(result_to_condition(
            result,
            ConditionSpec {
                condition_type: condition_types::SEERR_SYNC_READY,
                ok_reason: "SyncComplete",
                ok_message: "Sonarr and Radarr servers synced into Seerr",
                fail_reason: "SyncFailed",
                fail_log: "Seerr sync failed",
            },
            &name,
            &now,
        ))
    } else {
        None
    };

    // Bazarr cross-app sync
    let bazarr_sync_condition = if app.spec.app == AppType::Bazarr
        && let Some(ref sync_spec) = app.spec.bazarr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        let now = chrono_now();
        let result = sync_bazarr_apps(client, &app, target_ns).await;
        Some(result_to_condition(
            result,
            ConditionSpec {
                condition_type: condition_types::BAZARR_SYNC_READY,
                ok_reason: "SyncComplete",
                ok_message: "Sonarr and Radarr configured in Bazarr",
                fail_reason: "SyncFailed",
                fail_log: "Bazarr sync failed",
            },
            &name,
            &now,
        ))
    } else {
        None
    };

    // Subgen → Jellyfin sync
    let subgen_sync_condition = if app.spec.app == AppType::Subgen
        && let Some(ref sync_spec) = app.spec.subgen_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        let now = chrono_now();
        let result = sync_subgen_jellyfin(client, &app, target_ns).await;
        Some(result_to_condition(
            result,
            ConditionSpec {
                condition_type: condition_types::SUBGEN_SYNC_READY,
                ok_reason: "SyncComplete",
                ok_message: "Jellyfin env vars injected into Subgen Deployment",
                fail_reason: "SyncFailed",
                fail_log: "Subgen Jellyfin sync failed",
            },
            &name,
            &now,
        ))
    } else {
        None
    };

    // Maintainerr cross-app sync (only for Maintainerr-type apps with sync enabled)
    let maintainerr_sync_condition = if app.spec.app == AppType::Maintainerr
        && let Some(ref sync_spec) = app.spec.maintainerr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        let now = chrono_now();
        let result = sync_maintainerr_servers(client, &app, target_ns, None).await;
        Some(result_to_condition(
            result,
            ConditionSpec {
                condition_type: condition_types::MAINTAINERR_SYNC_READY,
                ok_reason: "SyncComplete",
                ok_message: "Sonarr, Radarr, Seerr, Tautulli, and Plex synced into Maintainerr",
                fail_reason: "SyncFailed",
                fail_log: "Maintainerr sync failed",
            },
            &name,
            &now,
        ))
    } else {
        None
    };

    // Download-client data health (Transmission only, non-blocking)
    let download_data_condition =
        check_download_client_health(&app, &recorder, &obj_ref, transmission_access.as_ref()).await;

    // Update status
    tracing::debug!(%name, "updating status");
    update_status(
        client,
        &app,
        StatusConditions {
            health: health_condition,
            update: update_condition,
            admin_creds: admin_creds_condition,
            bazarr_sync: bazarr_sync_condition,
            subgen_sync: subgen_sync_condition,
            prowlarr_sync: prowlarr_sync_condition,
            seerr_sync: seerr_sync_condition,
            maintainerr_sync: maintainerr_sync_condition,
            restore: restore_condition,
            download_data: download_data_condition,
        },
        backup_status,
    )
    .await?;

    info!(%name, "reconciliation complete");

    let duration = start_time.elapsed().as_secs_f64();
    observe_reconcile_duration(app_type, duration);
    increment_reconcile_total(app_type, "success");

    // Update managed-apps gauge from informer cache
    let gauge_api = if let Some(ref ns) = ctx.watch_namespace {
        Api::<ServarrApp>::namespaced(client.clone(), ns)
    } else {
        Api::<ServarrApp>::all(client.clone())
    };
    if let Ok(app_list) = gauge_api.list(&kube::api::ListParams::default()).await {
        let mut counts: std::collections::HashMap<(String, String), i64> =
            std::collections::HashMap::new();
        for a in &app_list.items {
            let key = (
                a.spec.app.as_str().to_owned(),
                a.namespace().unwrap_or_default(),
            );
            *counts.entry(key).or_default() += 1;
        }
        for ((t, n), count) in &counts {
            set_managed_apps(t, n, *count);
        }
    }

    recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "ReconcileSuccess".into(),
                note: Some(format!("All resources reconciled in {duration:.2}s")),
                action: "Reconcile".into(),
                secondary: None,
            },
            &obj_ref,
        )
        .await
        .map_err(Error::Kube)?;

    // Use a short requeue interval when admin credential sync is still pending so
    // the operator retries quickly once the app finishes starting up.
    let requeue_secs = if admin_creds_pending { 30 } else { 300 };
    Ok(Action::requeue(Duration::from_secs(requeue_secs)))
}

/// Create the API key Secret the first time `apiKeySecret` is reconciled.
///
/// A random 32-byte (64-char hex) key is generated and stored as `api-key`
/// in the Secret.  For .NET-based apps (Sonarr, Radarr, Lidarr, Prowlarr)
/// the deployment builder injects the value as the `APP__AUTH__APIKEY` env
/// var so the app uses the operator-managed key from first startup.
///
/// The Secret is owned by the ServarrApp so it is garbage-collected when the
/// ServarrApp is deleted.  An existing Secret is never touched.
async fn ensure_api_key_secret(client: &Client, app: &ServarrApp) -> Result<(), Error> {
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let ns = ns.as_str();
    // For Bazarr, the operator always manages the API key secret using a
    // deterministic name (<app-name>-api-key), regardless of apiKeySecret spec.
    let (secret_name, is_bazarr) = if matches!(app.spec.app, AppType::Bazarr) {
        (servarr_resources::common::child_name(app, "api-key"), true)
    } else {
        match app.spec.api_key_secret.as_deref() {
            Some(s) => (s.to_string(), false),
            None => return Ok(()),
        }
    };

    let secret_api = Api::<Secret>::namespaced(client.clone(), ns);

    // Only create if the Secret does not already exist.
    match secret_api.get(&secret_name).await {
        Ok(_) => return Ok(()),
        Err(kube::Error::Api(err)) if err.code == 404 => {}
        Err(e) => return Err(Error::Kube(e)),
    }

    use rand::distr::SampleString as _;
    let key = rand::distr::Alphanumeric.sample_string(&mut rand::rng(), 32);

    let secret = if is_bazarr {
        // Build the secret directly — child_name-based, not tied to api_key_secret field.
        Secret {
            metadata: servarr_resources::common::metadata(app, "api-key"),
            string_data: Some(std::collections::BTreeMap::from([("api-key".into(), key)])),
            type_: Some("Opaque".into()),
            ..Default::default()
        }
    } else if let Some(s) = servarr_resources::secret::build_api_key(app, &key) {
        s
    } else {
        return Ok(());
    };

    info!(name = %app.name_any(), secret = %secret_name, "creating api-key secret");
    secret_api
        .create(&PostParams::default(), &secret)
        .await
        .map_err(Error::Kube)?;

    Ok(())
}

/// Patch a SHA-256 checksum of the admin credentials onto the pod template annotation.
///
/// When the referenced Secret rotates, the annotation changes, which causes
/// Kubernetes to perform a rolling update of the Deployment so pods restart
/// with the new `secretKeyRef` env var values.
async fn patch_admin_credentials_checksum(
    client: &Client,
    app: &ServarrApp,
    secret_name: &str,
) -> Result<(), Error> {
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let ns = ns.as_str();
    use sha2::{Digest, Sha256};

    // Fetch Secret metadata to get resourceVersion (changes on every update).
    // Hash the resourceVersion instead of credentials to avoid leaking
    // a crackable, offline-attackable fingerprint of the secret value.
    let secret_api = Api::<Secret>::namespaced(client.clone(), ns);
    let secret = match secret_api.get(secret_name).await {
        Ok(s) => s,
        Err(e) => {
            warn!(
                app = %app.name_any(),
                secret = %secret_name,
                error = %kube_err_summary(&e),
                "admin-credentials: failed to fetch secret for checksum"
            );
            return Ok(());
        }
    };

    let resource_version = match secret.metadata.resource_version {
        Some(rv) => rv,
        None => {
            warn!(
                app = %app.name_any(),
                secret = %secret_name,
                "admin-credentials: Secret missing resourceVersion"
            );
            return Ok(());
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(resource_version.as_bytes());
    let checksum = hex::encode(hasher.finalize());

    let name = app.name_any();
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), ns);
    // Use a separate field manager so this annotation does not conflict with
    // the main SSA apply (FIELD_MANAGER), which would strip it on the next cycle.
    let pp = PatchParams::apply("servarr-operator/admin-credentials").force();
    let patch = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": name },
        "spec": {
            "template": {
                "metadata": {
                    "annotations": {
                        "servarr.dev/admin-credentials-checksum": checksum
                    }
                }
            }
        }
    });
    deploy_api
        .patch(&name, &pp, &Patch::Apply(patch))
        .await
        .map_err(Error::Kube)?;

    Ok(())
}

/// Sync admin credentials to apps that support live credential updates.
///
/// Servarr v3 apps (Sonarr/Radarr/Lidarr/Prowlarr) receive credentials via env
/// vars at startup — handled by the deployment builder and checksum annotation.
/// This function handles the remaining apps via their respective APIs.
///
/// This is idempotent and safe to call on every reconcile cycle.
async fn sync_admin_credentials(
    client: &Client,
    app: &ServarrApp,
    transmission_access: Option<&Result<TransmissionAccess, String>>,
) -> Option<Condition> {
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let ns = ns.as_str();
    let ac = app.spec.admin_credentials.as_ref()?;
    let now = chrono_now();

    let username = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "username").await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(
                app = %app.name_any(), error = %e.log_summary(),
                "admin-credentials: failed to read username"
            );
            return Some(Condition {
                condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED.to_string(),
                status: "Unknown".to_string(),
                reason: "SecretReadError".to_string(),
                message: e.public_summary(),
                last_transition_time: now,
            });
        }
    };
    let password = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "password").await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(
                app = %app.name_any(), error = %e.log_summary(),
                "admin-credentials: failed to read password"
            );
            return Some(Condition {
                condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED.to_string(),
                status: "Unknown".to_string(),
                reason: "SecretReadError".to_string(),
                message: e.public_summary(),
                last_transition_time: now,
            });
        }
    };

    let app_name = servarr_resources::common::service_name(app);
    let defaults = match servarr_crds::AppDefaults::for_app(&app.spec.app) {
        Ok(d) => d,
        Err(e) => {
            warn!(app = %app.name_any(), error = %e, "sync_admin_credentials: failed to load app defaults");
            return Some(Condition {
                condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED.to_string(),
                status: "Unknown".to_string(),
                reason: "DefaultsLoadError".to_string(),
                message: e.to_string(),
                last_transition_time: now,
            });
        }
    };
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    let result: Result<(), String> = match app.spec.app {
        AppType::Sabnzbd => {
            let api_key = match app.spec.api_key_secret.as_deref() {
                Some(s) => match servarr_api::read_secret_key(client, ns, s, "api-key").await {
                    Ok(k) => k,
                    Err(e) => {
                        return Some(Condition {
                            condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED
                                .to_string(),
                            status: "Unknown".to_string(),
                            reason: "ApiKeyReadError".to_string(),
                            message: e.public_summary(),
                            last_transition_time: now,
                        });
                    }
                },
                None => {
                    return Some(Condition::fail(
                        condition_types::ADMIN_CREDENTIALS_CONFIGURED,
                        "NoApiKey",
                        "SABnzbd credential sync requires apiKeySecret to be set",
                        &now,
                    ));
                }
            };
            match servarr_api::SabnzbdClient::new(&base_url, &api_key) {
                Ok(c) => c
                    .set_credentials(&username, &password)
                    .await
                    .map_err(|e| e.log_summary()),
                Err(e) => Err(e.log_summary()),
            }
        }
        AppType::Transmission => {
            // Try to enable auth without credentials first (Transmission starts with auth
            // disabled when LSIO's env var mechanism doesn't fire).  If we get 401,
            // auth is already enabled (e.g., by LSIO or a previous reconcile) and our
            // credentials should already be correct; confirm by fetching session info.
            info!(app = %app.name_any(), url = %base_url, "admin-credentials: syncing Transmission RPC auth");
            match servarr_api::TransmissionClient::new(&base_url, None) {
                Ok(c_no_auth) => match c_no_auth.session_set_auth(&username, &password).await {
                    Ok(()) => {
                        info!(app = %app.name_any(), "admin-credentials: Transmission session-set succeeded (auth now enabled)");
                        Ok(())
                    }
                    Err(servarr_api::ApiError::ApiResponse { status: 401, .. }) => {
                        info!(app = %app.name_any(), "admin-credentials: Transmission auth already enabled, verifying credentials");
                        match transmission_verify_client(
                            transmission_access,
                            &base_url,
                            &username,
                            &password,
                        ) {
                            Ok(c_auth) => c_auth
                                .session_get()
                                .await
                                .map(|_| ())
                                .map_err(|e| e.log_summary()),
                            Err(e) => Err(e.log_summary()),
                        }
                    }
                    Err(e) => {
                        warn!(app = %app.name_any(), error = %e.log_summary(), "admin-credentials: Transmission session-set failed");
                        Err(e.log_summary())
                    }
                },
                Err(e) => Err(e.log_summary()),
            }
        }
        AppType::Jellyfin => match servarr_api::JellyfinClient::new(&base_url) {
            Ok(c) => c
                .configure_admin(&username, &password)
                .await
                .map_err(|e| e.log_summary()),
            Err(e) => Err(e.log_summary()),
        },
        AppType::Tautulli => match servarr_api::TautulliClient::new(&base_url) {
            Ok(c) => c
                .set_credentials(&username, &password)
                .await
                .map_err(|e| e.log_summary()),
            Err(e) => Err(e.log_summary()),
        },
        AppType::Seerr => {
            let api_key = match app.spec.api_key_secret.as_deref() {
                Some(s) => match servarr_api::read_secret_key(client, ns, s, "api-key").await {
                    Ok(k) => k,
                    Err(e) => {
                        return Some(Condition {
                            condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED
                                .to_string(),
                            status: "Unknown".to_string(),
                            reason: "ApiKeyReadError".to_string(),
                            message: e.public_summary(),
                            last_transition_time: now,
                        });
                    }
                },
                None => {
                    return Some(Condition::fail(
                        condition_types::ADMIN_CREDENTIALS_CONFIGURED,
                        "NoApiKey",
                        "Seerr credential sync requires apiKeySecret to be set",
                        &now,
                    ));
                }
            };
            let c = servarr_api::SeerrClient::new(&base_url, &api_key);
            c.setup_local_auth(&username, &password)
                .await
                .map_err(|e| e.log_summary())
        }
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr => {
            let api_key = match app.spec.api_key_secret.as_deref() {
                Some(s) => match servarr_api::read_secret_key(client, ns, s, "api-key").await {
                    Ok(k) => k,
                    Err(e) => {
                        return Some(Condition {
                            condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED
                                .to_string(),
                            status: "Unknown".to_string(),
                            reason: "ApiKeyReadError".to_string(),
                            message: e.public_summary(),
                            last_transition_time: now,
                        });
                    }
                },
                None => String::new(),
            };
            let app_kind = app_type_to_kind(&app.spec.app)?;
            match servarr_api::ServarrClient::new(&base_url, &api_key, app_kind) {
                Ok(c) => match c.configure_admin(&username, &password).await {
                    Ok(()) => Ok(()),
                    Err(servarr_api::ApiError::ApiResponse { status: 401, .. }) => {
                        // Auth is already enabled and we have no valid API key to reach it.
                        // This can happen if the pod started with stale auth env vars or was
                        // configured out-of-band.  Leave the condition unchanged; the operator
                        // will retry on the next reconcile (triggered by pod/Deployment events).
                        warn!(app = %app.name_any(), "admin-credentials: configure_admin returned 401 — auth already active, no api key");
                        return None;
                    }
                    Err(e) => Err(e.log_summary()),
                },
                Err(e) => Err(e.log_summary()),
            }
        }
        AppType::Bazarr => {
            // Read the operator-managed API key for Bazarr
            let api_key_secret = servarr_resources::common::child_name(app, "api-key");
            let api_key = match servarr_api::read_secret_key(client, ns, &api_key_secret, "api-key")
                .await
            {
                Ok(k) => k,
                Err(e) => {
                    return Some(Condition {
                        condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED.to_string(),
                        status: "Unknown".to_string(),
                        reason: "ApiKeyReadError".to_string(),
                        message: e.public_summary(),
                        last_transition_time: now,
                    });
                }
            };
            match servarr_api::BazarrClient::new(&base_url, &api_key) {
                Ok(c) => {
                    let password_md5 = format!("{:x}", md5::compute(password.as_bytes()));
                    c.set_credentials(&username, &password_md5)
                        .await
                        .map_err(|e| e.log_summary())
                }
                Err(e) => Err(e.log_summary()),
            }
        }
        // Plex: uses plex.tv account auth, not configurable via operator
        // Maintainerr: no credential API exposed
        _ => return None,
    };

    Some(match result {
        Ok(()) => Condition::ok(
            condition_types::ADMIN_CREDENTIALS_CONFIGURED,
            "Configured",
            "Admin credentials applied successfully",
            &now,
        ),
        Err(ref msg) => {
            warn!(app = %app.name_any(), error = %msg, "admin-credentials: sync failed");
            Condition::fail(
                condition_types::ADMIN_CREDENTIALS_CONFIGURED,
                "SyncFailed",
                msg,
                &now,
            )
        }
    })
}

/// Choose the Transmission client for `sync_admin_credentials`'s 401→verify fallback.
/// Reuses the shared per-reconcile client when it is authenticated (credential sync and the
/// health checks read the same adminCredentials secret, so its session-ID cache is already
/// warm), avoiding a redundant handshake; otherwise builds a fresh authenticated client from
/// the just-read credentials (#508).
fn transmission_verify_client(
    transmission_access: Option<&Result<TransmissionAccess, String>>,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<servarr_api::TransmissionClient, servarr_api::ApiError> {
    match transmission_access {
        Some(Ok(access)) if !access.credentials_incomplete => Ok(access.client.clone()),
        _ => servarr_api::TransmissionClient::new(
            base_url,
            Some(&servarr_api::BasicCredentials::new(username, password)),
        ),
    }
}

/// Base URL for `app`'s in-cluster Service, honoring `spec.service`/the app-type default port.
fn resolve_service_base_url(app: &ServarrApp, ns: &str) -> Result<String, String> {
    let app_name = servarr_resources::common::service_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app)?;
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    Ok(format!("http://{app_name}.{ns}.svc:{port}"))
}

/// Read Transmission's optional adminCredentials secret for RPC authentication. A missing
/// secret or unreadable key logs a warning and falls back to unauthenticated access rather
/// than failing the caller outright.
async fn read_transmission_admin_creds(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
) -> (Option<String>, Option<String>) {
    let Some(ref ac) = app.spec.admin_credentials else {
        return (None, None);
    };
    let u = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "username").await {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(app = %app.name_any(), error = %e.log_summary(),
                    "health-check: failed to read Transmission username, proceeding unauthenticated");
            None
        }
    };
    let p = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "password").await {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(app = %app.name_any(), error = %e.log_summary(),
                    "health-check: failed to read Transmission password, proceeding unauthenticated");
            None
        }
    };
    (u, p)
}

/// Whether `app` is a Transmission app with `apiHealthCheck.enabled` set -- the gate shared by
/// `reconcile`'s `transmission_access` resolution and `check_download_client_health`.
fn transmission_health_check_enabled(app: &ServarrApp) -> bool {
    app.spec.app == AppType::Transmission
        && app
            .spec
            .api_health_check
            .as_ref()
            .is_some_and(|hc| hc.enabled)
}

/// `reconcile` only omits `transmission_access` when `apiHealthCheck.enabled` is false, but
/// both of its consumers below re-check that gate themselves before consulting it -- so this
/// message should never actually surface. Shared so the two defensive branches don't drift.
const TRANSMISSION_CLIENT_UNRESOLVED: &str =
    "Transmission client was not resolved for this reconcile";

/// Default for `apiHealthCheck.intervalSeconds` when omitted — matches the documented
/// contract "Defaults to 60" (#506).
const DEFAULT_HEALTH_CHECK_INTERVAL_SECONDS: u32 = 60;

/// Transmission client + admin-credential state resolved once per reconcile pass and shared
/// between `check_api_health`'s Transmission arm and `check_download_client_health` — both gate
/// on the same `apiHealthCheck.enabled` flag, so building this independently in each would mean
/// double the Secret GETs and a redundant session-ID handshake against the same app (#499).
#[derive(Debug)]
pub(crate) struct TransmissionAccess {
    client: servarr_api::TransmissionClient,
    /// `true` when `adminCredentials` was configured but at least one of username/password
    /// could not be read (partial or total read failure). The read-only health probe below
    /// degrades to unauthenticated in this case; the destructive download-health path fails
    /// closed instead (#483 hardening) — this flag lets each caller apply its own policy
    /// against the same resolved credentials.
    credentials_incomplete: bool,
}

/// Resolve a [`TransmissionAccess`] for `app`: read the optional adminCredentials secret and
/// build a `TransmissionClient` against `base_url`. Called once per reconcile (see `reconcile`)
/// so both health checks share the same client instance instead of each triggering their own
/// session-ID handshake (#499).
async fn resolve_transmission_access(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    base_url: &str,
) -> Result<TransmissionAccess, String> {
    let (tx_user, tx_pass) = read_transmission_admin_creds(client, app, ns).await;
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
    })
}

/// Return the app's current status condition of `condition_type`, if any.
fn current_condition<'a>(app: &'a ServarrApp, condition_type: &str) -> Option<&'a Condition> {
    app.status.as_ref().and_then(|s| {
        s.conditions
            .iter()
            .find(|c| c.condition_type == condition_type)
    })
}

/// `apiHealthCheck.intervalSeconds` throttle (#506): whether the health poll for
/// `condition_type` should be skipped because the existing condition's
/// `lastTransitionTime` still falls inside `interval_seconds` of now.
///
/// Only a positive health assertion (`status == "True"`) is rate-limited. An error,
/// `Unknown`, or `False` condition is never throttled, so recovery is re-probed on the
/// next reconcile instead of being frozen until the interval elapses. A missing or
/// unparseable timestamp, a future-dated timestamp, or an unset/zero interval never
/// throttles — the poll runs.
fn is_health_poll_throttled(
    existing: Option<&Condition>,
    interval_seconds: Option<u32>,
    now: &str,
) -> bool {
    let interval = interval_seconds.unwrap_or(DEFAULT_HEALTH_CHECK_INTERVAL_SECONDS);
    let Some(existing) = existing else {
        return false;
    };
    if existing.status != "True" {
        return false;
    }
    let (Ok(last), Ok(current)) = (
        chrono::DateTime::parse_from_rfc3339(&existing.last_transition_time),
        chrono::DateTime::parse_from_rfc3339(now),
    ) else {
        return false;
    };
    // Fail open on a future-dated timestamp (clock skew, hand-edit): `current < last`
    // would otherwise throttle every poll — including at `intervalSeconds: 0` — until
    // the future passes.
    if current < last {
        return false;
    }
    current - last < chrono::Duration::seconds(i64::from(interval))
}

pub(crate) async fn check_api_health(
    client: &Client,
    app: &ServarrApp,
    transmission_access: Option<&Result<TransmissionAccess, String>>,
) -> (Option<Condition>, Option<Condition>) {
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let ns = ns.as_str();
    let health_check = match app.spec.api_health_check.as_ref() {
        Some(hc) if hc.enabled => hc,
        _ => return (None, None),
    };

    let now = chrono_now();

    // `intervalSeconds` throttle (#506): while the existing APP_HEALTHY condition is still
    // inside its poll window, skip the API probe entirely. The existing health AND update
    // conditions are kept frozen, so the throttle doesn't erase the update-available signal
    // from status between polls.
    let existing_health = current_condition(app, condition_types::APP_HEALTHY);
    if is_health_poll_throttled(existing_health, health_check.interval_seconds, &now) {
        // A skip is intentional (rate-limit window), but it must be observable — otherwise a
        // stale health status looks indistinguishable from a healthy app that's just idle.
        debug!(
            app = %app.name_any(),
            condition = condition_types::APP_HEALTHY,
            interval_seconds = health_check
                .interval_seconds
                .unwrap_or(DEFAULT_HEALTH_CHECK_INTERVAL_SECONDS),
            "api health poll throttled inside intervalSeconds window; keeping existing condition"
        );
        return (
            existing_health.cloned(),
            current_condition(app, condition_types::UPDATE_AVAILABLE).cloned(),
        );
    }

    let base_url = match resolve_service_base_url(app, ns) {
        Ok(u) => u,
        Err(e) => {
            warn!(error = %e, "check_api_health: failed to load app defaults");
            let cond = Condition {
                condition_type: condition_types::APP_HEALTHY.to_string(),
                status: "Unknown".to_string(),
                reason: "DefaultsLoadError".to_string(),
                message: e,
                last_transition_time: now,
            };
            return (Some(cond), None);
        }
    };

    use servarr_api::HealthCheck;
    let (healthy, update_cond): (Result<bool, String>, Option<Condition>) = match app.spec.app {
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr => {
            let Some(app_kind) = app_type_to_kind(&app.spec.app) else {
                return (None, None);
            };
            // These arms authenticate with an API key, so apiKeySecret is still required
            // here — unlike the Transmission/Jellyfin/Plex arms below, which depend only on
            // apiHealthCheck.enabled (#509).
            let api_key = match resolve_health_api_key(client, ns, app, &now).await {
                Ok(Some(k)) => k,
                Ok(None) => return (None, None),
                Err(cond) => return (cond, None),
            };
            match servarr_api::ServarrClient::new(&base_url, &api_key, app_kind) {
                Ok(c) => {
                    let h = c.is_healthy().await.map_err(|e| e.log_summary());
                    let uc = check_update_available(&c, &now).await;
                    (h, uc)
                }
                Err(e) => (Err(e.log_summary()), None),
            }
        }
        AppType::Sabnzbd => {
            // SABnzbd also authenticates with an API key, so apiKeySecret is required here
            // too (#509).
            let api_key = match resolve_health_api_key(client, ns, app, &now).await {
                Ok(Some(k)) => k,
                Ok(None) => return (None, None),
                Err(cond) => return (cond, None),
            };
            match servarr_api::SabnzbdClient::new(&base_url, &api_key) {
                Ok(c) => {
                    let h = c.is_healthy().await.map_err(|e| e.log_summary());
                    (h, None)
                }
                Err(e) => (Err(e.log_summary()), None),
            }
        }
        AppType::Transmission => match transmission_access {
            Some(Ok(access)) => {
                let h = access
                    .client
                    .is_healthy()
                    .await
                    .map_err(|e| e.log_summary());
                (h, None)
            }
            Some(Err(e)) => (Err(e.clone()), None),
            None => (Err(TRANSMISSION_CLIENT_UNRESOLVED.to_string()), None),
        },
        AppType::Jellyfin => match servarr_api::JellyfinClient::new(&base_url) {
            Ok(c) => {
                let h = c.is_healthy().await.map_err(|e| e.log_summary());
                (h, None)
            }
            Err(e) => (Err(e.log_summary()), None),
        },
        AppType::Plex => match servarr_api::PlexClient::new(&base_url) {
            Ok(c) => {
                let h = c.is_healthy().await.map_err(|e| e.log_summary());
                (h, None)
            }
            Err(e) => (Err(e.log_summary()), None),
        },
        _ => return (None, None),
    };

    let health_cond = match healthy {
        Ok(true) => Condition::ok(
            condition_types::APP_HEALTHY,
            "Healthy",
            "API responded healthy",
            &now,
        ),
        Ok(false) => Condition::fail(
            condition_types::APP_HEALTHY,
            "Unhealthy",
            "API responded unhealthy",
            &now,
        ),
        Err(msg) => Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "Unknown".to_string(),
            reason: "ApiError".to_string(),
            message: msg,
            last_transition_time: now,
        },
    };

    (Some(health_cond), update_cond)
}

/// Resolve the API key for the health-check arms that authenticate with one (Sonarr/Radarr/
/// Lidarr/Prowlarr/Sabnzbd). Returns `Ok(Some(key))` on success, `Ok(None)` when no
/// apiKeySecret is configured (caller reports no health condition, preserving the pre-#509
/// behavior for these app types), or `Err(cond)` when the Secret read failed (caller reports
/// an Unknown APP_HEALTHY condition).
async fn resolve_health_api_key(
    client: &Client,
    ns: &str,
    app: &ServarrApp,
    now: &str,
) -> Result<Option<String>, Option<Condition>> {
    let Some(secret_name) = app.spec.api_key_secret.as_deref() else {
        return Ok(None);
    };
    match servarr_api::read_secret_key(client, ns, secret_name, "api-key").await {
        Ok(k) => Ok(Some(k)),
        Err(e) => {
            warn!(error = %e.log_summary(), "failed to read API key secret");
            Err(Some(Condition {
                condition_type: condition_types::APP_HEALTHY.to_string(),
                status: "Unknown".to_string(),
                reason: "SecretReadError".to_string(),
                message: e.public_summary(),
                last_transition_time: now.to_string(),
            }))
        }
    }
}

async fn check_update_available(
    client: &servarr_api::ServarrClient,
    now: &str,
) -> Option<Condition> {
    let updates = match client.updates().await {
        Ok(u) => u,
        Err(e) => {
            tracing::debug!(error = %e.log_summary(), "failed to fetch updates, skipping update condition");
            return None;
        }
    };

    let available = updates.iter().find(|u| !u.installed && u.installable);
    Some(match available {
        Some(update) => Condition::ok(
            condition_types::UPDATE_AVAILABLE,
            "UpdateAvailable",
            &format!("Version {} is available", update.version),
            now,
        ),
        None => Condition::fail(
            condition_types::UPDATE_AVAILABLE,
            "UpToDate",
            "Running latest version",
            now,
        ),
    })
}

/// Substring (matched case-insensitively) Transmission reports in `errorString` when the
/// on-disk data for a torrent has gone missing — e.g. an external cleanup job deleted files
/// Transmission still references in its session (#483).
const MISSING_DATA_ERROR_PATTERN: &str = "no data found";

/// How many times to re-poll a torrent's status after triggering `torrent-verify` before
/// treating it as "still checking, try again next reconcile" rather than removing it.
/// Transmission clears the checking status for a genuinely-missing (zero-byte) torrent in
/// well under a second, so a handful of short polls is enough without blocking reconcile
/// for a long-running verify of a torrent that turns out to be fine.
const VERIFY_POLL_ATTEMPTS: u8 = 5;
const VERIFY_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Transmission RPC status codes meaning "still hash-checking" (queued-to-verify, verifying).
const CHECKING_STATUSES: [i64; 2] = [1, 2];

/// `torrent-get` fields needed for self-heal detection and polling. A single shared list (not
/// duplicated per call site) so `hashString` can never silently drop from one of the two calls —
/// a `TorrentInfo` with a missing hash defaults to `""` (#500), which would scope subsequent
/// `torrent-verify`/`torrent-remove`/`torrent-get` calls to an empty-string id instead of failing
/// loudly.
const TORRENT_HEALTH_FIELDS: [&str; 6] =
    ["id", "name", "error", "errorString", "status", "hashString"];

/// Transmission's `tr_stat` error code for "the torrent has a local I/O problem" (missing or
/// unreadable data, permissions, etc.). Codes `1`/`2` are tracker warning/error and carry
/// `errorString` text controlled by the torrent's tracker, not the local filesystem — matching
/// on those would let a hostile tracker trigger removal of a perfectly healthy torrent by
/// returning "no data found" in a `failure reason`/`warning message` (#483).
const TR_STAT_LOCAL_ERROR: i64 = 3;

/// Build an `Unknown` `DownloadDataHealthy` condition — used for every non-destructive
/// failure path in [`check_download_client_health`] so a swallowed error still surfaces on
/// the ServarrApp status instead of the condition silently disappearing (#483).
fn download_health_unknown(reason: &str, message: String, now: &str) -> Condition {
    Condition {
        condition_type: condition_types::DOWNLOAD_DATA_HEALTHY.to_string(),
        status: "Unknown".to_string(),
        reason: reason.to_string(),
        message,
        last_transition_time: now.to_string(),
    }
}

fn is_missing_data_torrent(t: &servarr_api::TorrentInfo) -> bool {
    t.error == TR_STAT_LOCAL_ERROR
        && t.error_string
            .to_lowercase()
            .contains(MISSING_DATA_ERROR_PATTERN)
}

fn is_checking(t: &servarr_api::TorrentInfo) -> bool {
    CHECKING_STATUSES.contains(&t.status)
}

/// Outcome of attempting to remediate a batch of stale torrents, addressed by their stable
/// `hashString` (#500): which were removed as confirmed-orphaned, which were confirmed orphaned
/// but left alone because `auto_remove` is disabled (#498), and which are still hash-checking
/// (retried on the next reconcile).
#[derive(Default)]
struct RemediationOutcome {
    removed: Vec<String>,
    confirmed_orphaned: Vec<String>,
    still_pending: Vec<String>,
}

/// Detect Transmission torrents whose on-disk data has gone missing, trigger a verify so
/// Transmission re-checks, and remove torrents confirmed still broken once the verify
/// settles (#483). Gated behind `apiHealthCheck.enabled` since it needs the same live RPC
/// access as [`check_api_health`]; only applies to Transmission apps. Removal additionally
/// requires `apiHealthCheck.autoRemove` (#498).
async fn check_download_client_health(
    app: &ServarrApp,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    transmission_access: Option<&Result<TransmissionAccess, String>>,
) -> Option<Condition> {
    if app.spec.app != AppType::Transmission {
        return None;
    }
    let health_check = match app.spec.api_health_check.as_ref() {
        Some(hc) if hc.enabled => hc,
        _ => return None,
    };
    let now = chrono_now();

    // `intervalSeconds` throttle (#506): while the existing DOWNLOAD_DATA_HEALTHY condition
    // is still inside its poll window, skip the self-heal pass entirely. This is the fix the
    // throttle primarily exists for — a user setting `intervalSeconds: 3600` next to
    // `autoRemoveOrphanedTorrents: true` expects the destructive torrent-remove check at most
    // hourly, not on every reconcile.
    let existing_download = current_condition(app, condition_types::DOWNLOAD_DATA_HEALTHY);
    if is_health_poll_throttled(existing_download, health_check.interval_seconds, &now) {
        // A skip is intentional (rate-limit window), but it must be observable — otherwise the
        // self-heal pass pausing (including destructive torrent-remove checks) looks like a
        // hang. This branch returns before the access match, so a broken client/credential
        // surfaced here would otherwise be invisible until the window elapses.
        debug!(
            app = %app.name_any(),
            condition = condition_types::DOWNLOAD_DATA_HEALTHY,
            interval_seconds = health_check
                .interval_seconds
                .unwrap_or(DEFAULT_HEALTH_CHECK_INTERVAL_SECONDS),
            "download client health poll throttled inside intervalSeconds window; keeping existing condition"
        );
        return existing_download.cloned();
    }
    let auto_remove = health_check.auto_remove_orphaned_torrents;

    let access = match transmission_access {
        Some(Ok(a)) => a,
        Some(Err(e)) => return Some(download_health_unknown("ClientBuildError", e.clone(), &now)),
        None => {
            return Some(download_health_unknown(
                "ClientBuildError",
                TRANSMISSION_CLIENT_UNRESOLVED.to_string(),
                &now,
            ));
        }
    };
    // access.client is built from the same credential read `check_api_health`'s read-only probe
    // uses, but this path can delete torrents, so fail closed instead of silently proceeding
    // unauthenticated when adminCredentials was configured but couldn't be fully read (#483).
    if access.credentials_incomplete {
        warn!(app = %app.name_any(),
            "download-client health: adminCredentials secret is missing username and/or \
             password, refusing to proceed unauthenticated on a destructive path");
        return Some(download_health_unknown(
            "CredentialReadError",
            "adminCredentials secret is missing username or password".to_string(),
            &now,
        ));
    }
    let tx_client = &access.client;

    let torrents = match tx_client.torrent_get(&TORRENT_HEALTH_FIELDS, None).await {
        Ok(t) => t,
        Err(e) => {
            warn!(app = %app.name_any(), error = %e.log_summary(),
                    "download-client health: torrent-get failed");
            return Some(download_health_unknown(
                "TorrentGetError",
                e.log_summary(),
                &now,
            ));
        }
    };

    let stale: Vec<_> = torrents
        .into_iter()
        .filter(is_missing_data_torrent)
        .filter(|t| {
            // hash_string defaults to "" (#[serde(default)]) if Transmission's response ever
            // omits hashString despite it being requested. Skip rather than address an empty
            // string via torrent-verify/torrent-remove -- an empty scope would misclassify as
            // "settled" (vacuously true over zero results) and leave the torrent silently stuck
            // in "still pending" forever instead of surfacing the anomaly (#500).
            let has_hash = !t.hash_string.is_empty();
            if !has_hash {
                warn!(app = %app.name_any(), torrent_id = t.id,
                    "download-client health: torrent reporting missing data has no hashString, \
                     skipping until Transmission reports one");
            }
            has_hash
        })
        .collect();
    if stale.is_empty() {
        return Some(Condition::ok(
            condition_types::DOWNLOAD_DATA_HEALTHY,
            "NoStaleData",
            "No torrents reporting missing data",
            &now,
        ));
    }

    let outcome = remediate_stale_torrents(tx_client, &stale, auto_remove).await;
    report_stale_torrents(recorder, obj_ref, &stale, &outcome).await;

    Some(build_download_health_condition(stale.len(), &outcome, &now))
}

/// Trigger `torrent-verify` on every stale torrent, wait for the verify to settle, then remove
/// any that are still reporting missing data -- Transmission's own re-check is the safety gate
/// that distinguishes "transient" from "genuinely gone" (#483). Torrents are addressed by their
/// stable `hashString`, not the process-local numeric `id`, which can be reassigned to a
/// different torrent if Transmission restarts inside the detect-to-remediate window (#500).
/// Removal is skipped -- confirmed-orphaned torrents are reported but left alone -- unless
/// `auto_remove` is set (#498).
async fn remediate_stale_torrents(
    tx_client: &servarr_api::TransmissionClient,
    stale: &[servarr_api::TorrentInfo],
    auto_remove: bool,
) -> RemediationOutcome {
    // Torrents Transmission is already hash-checking (e.g. verify was triggered on a
    // previous reconcile and hasn't finished) must not be re-verified — restarting the
    // hash-check on every 300s reconcile means a large torrent's verify never converges
    // and disk I/O never lets up (#483).
    let already_checking: Vec<String> = stale
        .iter()
        .filter(|t| is_checking(t))
        .map(|t| t.hash_string.clone())
        .collect();
    let hashes: Vec<String> = stale
        .iter()
        .filter(|t| !is_checking(t))
        .map(|t| t.hash_string.clone())
        .collect();
    if hashes.is_empty() {
        return RemediationOutcome {
            still_pending: already_checking,
            ..Default::default()
        };
    }
    let hash_refs: Vec<&str> = hashes.iter().map(String::as_str).collect();

    if let Err(e) = tx_client.torrent_verify(&hash_refs).await {
        warn!(error = %e.log_summary(), "download-client health: torrent-verify failed");
        let mut still_pending = hashes;
        still_pending.extend(already_checking);
        return RemediationOutcome {
            still_pending,
            ..Default::default()
        };
    }

    let settled = poll_until_settled(tx_client, &hash_refs).await;
    let still_erroring: Vec<String> = settled
        .iter()
        .filter(|t| is_missing_data_torrent(t))
        .map(|t| t.hash_string.clone())
        .collect();
    let mut still_checking: Vec<String> = hashes
        .into_iter()
        .filter(|h| !settled.iter().any(|t| &t.hash_string == h))
        .collect();
    still_checking.extend(already_checking);

    // still_erroring.is_empty() falls through to the branches below rather than an early
    // return: with auto_remove=false that's already `confirmed_orphaned: [] , still_pending:
    // still_checking`, and with auto_remove=true, torrent_remove(&[]) is a documented no-op
    // that produces the identical outcome -- both paths already agree, no need to special-case.
    if !auto_remove {
        return RemediationOutcome {
            confirmed_orphaned: still_erroring,
            still_pending: still_checking,
            ..Default::default()
        };
    }

    let still_erroring_refs: Vec<&str> = still_erroring.iter().map(String::as_str).collect();
    match tx_client
        .torrent_remove(&still_erroring_refs, servarr_api::DeleteLocalData::No)
        .await
    {
        Ok(()) => RemediationOutcome {
            removed: still_erroring,
            still_pending: still_checking,
            ..Default::default()
        },
        Err(e) => {
            warn!(error = %e.log_summary(), "download-client health: torrent-remove failed");
            let mut still_pending = still_erroring;
            still_pending.extend(still_checking);
            RemediationOutcome {
                still_pending,
                ..Default::default()
            }
        }
    }
}

/// Poll `torrent-get` for `hashes` until none are still hash-checking, or
/// [`VERIFY_POLL_ATTEMPTS`] is exhausted. Returns an empty `Vec` on timeout or RPC error —
/// callers treat that as "still pending", never as grounds for removal.
async fn poll_until_settled(
    tx_client: &servarr_api::TransmissionClient,
    hashes: &[&str],
) -> Vec<servarr_api::TorrentInfo> {
    // Transmission applies torrent-verify's status transition asynchronously relative to the
    // RPC response returning, so polling immediately risks reading the pre-verify snapshot —
    // still showing the old error with status 0 (not checking) — and wrongly treating the
    // torrent as "settled and still broken" before Transmission ever re-checked it (#483).
    // Sleep first to give the transition time to land.
    tokio::time::sleep(VERIFY_POLL_INTERVAL).await;
    for _ in 0..VERIFY_POLL_ATTEMPTS {
        match tx_client
            .torrent_get(&TORRENT_HEALTH_FIELDS, Some(hashes))
            .await
        {
            Ok(t) if t.iter().all(|x| !is_checking(x)) => return t,
            Ok(_) => tokio::time::sleep(VERIFY_POLL_INTERVAL).await,
            Err(e) => {
                warn!(error = %e.log_summary(), "download-client health: torrent-get (poll) failed");
                return Vec::new();
            }
        }
    }
    Vec::new()
}

async fn report_stale_torrents(
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    stale: &[servarr_api::TorrentInfo],
    outcome: &RemediationOutcome,
) {
    // Nothing needed remediation by the time this pass finished (e.g. every stale torrent
    // recovered during the verify poll) — no Warning worth surfacing.
    if outcome.removed.is_empty()
        && outcome.confirmed_orphaned.is_empty()
        && outcome.still_pending.is_empty()
    {
        return;
    }

    // Torrent names come from tracker/`.torrent` metadata outside the operator's trust
    // boundary, so they must not reach a tenant-visible Event unsanitized (control
    // characters, unbounded length — `events.k8s.io/v1` rejects a note over 1024 chars).
    // Report the operator-assigned ids instead; names are available in the operator's own
    // debug log for troubleshooting (#483).
    debug!(
        names = ?stale.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        "download-client health: stale torrent names"
    );
    let ids: Vec<i64> = stale.iter().map(|t| t.id).collect();
    let note = format!(
        "{} torrent(s) reporting missing data (ids: {ids:?}); {} removed as orphaned, \
         {} confirmed orphaned but not removed, {} pending verify",
        stale.len(),
        outcome.removed.len(),
        outcome.confirmed_orphaned.len(),
        outcome.still_pending.len(),
    );
    if let Err(e) = recorder
        .publish(
            &Event {
                type_: EventType::Warning,
                reason: "DownloadDataMissing".into(),
                note: Some(note),
                action: "DownloadClientHealthCheck".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await
    {
        warn!(error = %e, "download-client health: failed to publish event");
    }
}

fn build_download_health_condition(
    stale_count: usize,
    outcome: &RemediationOutcome,
    now: &str,
) -> Condition {
    let recovered = stale_count
        .saturating_sub(outcome.removed.len())
        .saturating_sub(outcome.confirmed_orphaned.len())
        .saturating_sub(outcome.still_pending.len());
    if recovered == stale_count {
        return Condition::ok(
            condition_types::DOWNLOAD_DATA_HEALTHY,
            "DataRecovered",
            &format!("{stale_count} torrent(s) recovered after verify"),
            now,
        );
    }
    Condition::fail(
        condition_types::DOWNLOAD_DATA_HEALTHY,
        "MissingDataDetected",
        &format!(
            "{stale_count} torrent(s) reporting missing data ({recovered} recovered, {} removed, \
             {} confirmed orphaned but not removed, {} pending verify)",
            outcome.removed.len(),
            outcome.confirmed_orphaned.len(),
            outcome.still_pending.len(),
        ),
        now,
    )
}

pub(crate) struct StatusConditions {
    pub health: Option<Condition>,
    pub update: Option<Condition>,
    pub admin_creds: Option<Condition>,
    /// Bazarr cross-app sync result (only set for Bazarr apps with sync enabled).
    pub bazarr_sync: Option<Condition>,
    /// Subgen → Jellyfin sync result (only set for Subgen apps with sync enabled).
    pub subgen_sync: Option<Condition>,
    /// Prowlarr cross-app sync result (only set for Prowlarr apps with sync enabled).
    pub prowlarr_sync: Option<Condition>,
    /// Seerr cross-app sync result (only set for Seerr apps with sync enabled).
    pub seerr_sync: Option<Condition>,
    /// Maintainerr cross-app sync result (only set for Maintainerr apps with sync enabled).
    pub maintainerr_sync: Option<Condition>,
    /// Backup restore result (only set when a restore was attempted this reconcile).
    pub restore: Option<Condition>,
    /// Download-client data health (only set for Transmission apps with health checking
    /// enabled — see [`check_download_client_health`]).
    pub download_data: Option<Condition>,
}

/// The condition vocabulary for a reconcile sub-step: the type plus the
/// reason/message strings for both outcomes and the failure log line. Grouping
/// them keeps [`result_to_condition`] within the positional-arg budget. (#15)
struct ConditionSpec<'a> {
    condition_type: &'a str,
    ok_reason: &'a str,
    ok_message: &'a str,
    fail_reason: &'a str,
    fail_log: &'a str,
}

/// Turn a reconcile sub-step `Result` into a status [`Condition`]: success
/// yields an `ok` condition, failure yields a `fail` condition carrying the
/// sanitized [`TenantSafeMessage`] and emits a `warn!` keyed on `name`. (#15)
///
/// The generic error must convert into a [`TenantSafeMessage`], so only
/// sanitizer output can ever reach the tenant-visible Condition message.
fn result_to_condition<E: Into<TenantSafeMessage>>(
    result: Result<(), E>,
    spec: ConditionSpec<'_>,
    name: &str,
    now: &str,
) -> Condition {
    match result {
        Ok(()) => Condition::ok(spec.condition_type, spec.ok_reason, spec.ok_message, now),
        Err(e) => {
            let msg: TenantSafeMessage = e.into();
            warn!(%name, error = %msg, "{}", spec.fail_log);
            Condition::fail(spec.condition_type, spec.fail_reason, msg.as_ref(), now)
        }
    }
}

pub(crate) async fn update_status(
    client: &Client,
    app: &ServarrApp,
    conditions: StatusConditions,
    backup_status: Option<servarr_crds::BackupStatus>,
) -> Result<(), Error> {
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let ns = ns.as_str();
    let name = app.name_any();
    let name = name.as_str();
    let StatusConditions {
        health: health_condition,
        update: update_condition,
        admin_creds: admin_creds_condition,
        bazarr_sync: bazarr_sync_condition,
        subgen_sync: subgen_sync_condition,
        prowlarr_sync: prowlarr_sync_condition,
        seerr_sync: seerr_sync_condition,
        maintainerr_sync: maintainerr_sync_condition,
        restore: restore_condition,
        download_data: download_data_condition,
    } = conditions;
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), ns);
    let (ready, ready_replicas) = match deploy_api.get(name).await {
        Ok(deploy) => {
            let replicas = deploy
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0);
            (replicas > 0, replicas)
        }
        Err(e) => {
            warn!(%name, error = %kube_err_summary(&e), "failed to get Deployment for status check, reporting not-ready");
            (false, 0)
        }
    };

    let generation = app.metadata.generation.unwrap_or(0);
    let now = chrono_now();
    let mut status = ServarrAppStatus {
        ready,
        ready_replicas,
        observed_generation: generation,
        conditions: Vec::new(),
        backup_status,
    };

    // DeploymentReady
    if ready {
        status.set_condition(Condition::ok(
            condition_types::DEPLOYMENT_READY,
            "ReplicasAvailable",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        ));
    } else {
        status.set_condition(Condition::fail(
            condition_types::DEPLOYMENT_READY,
            "ReplicasUnavailable",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        ));
    }

    // ServiceReady — we just applied it, so mark true
    status.set_condition(Condition::ok(
        condition_types::SERVICE_READY,
        "Applied",
        "Service applied",
        &now,
    ));

    // Progressing is false now (reconcile completed)
    status.set_condition(Condition::fail(
        condition_types::PROGRESSING,
        "ReconcileComplete",
        "Reconciliation finished",
        &now,
    ));

    // Overall Ready
    status.set_condition(if ready {
        Condition::ok(
            condition_types::READY,
            "DeploymentReady",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        )
    } else {
        Condition::fail(
            condition_types::READY,
            "DeploymentNotReady",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        )
    });

    // Degraded
    if !ready {
        status.set_condition(Condition::ok(
            condition_types::DEGRADED,
            "DeploymentNotReady",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        ));
    } else {
        status.set_condition(Condition::fail(
            condition_types::DEGRADED,
            "AllHealthy",
            "All resources healthy",
            &now,
        ));
    }

    // Optional sub-step conditions, applied in a stable order. Each is present
    // only when its reconcile sub-step ran (API health, update check, admin
    // creds, cross-app sync, restore). (#15)
    for cond in [
        health_condition,
        update_condition,
        admin_creds_condition,
        bazarr_sync_condition,
        subgen_sync_condition,
        prowlarr_sync_condition,
        seerr_sync_condition,
        maintainerr_sync_condition,
        restore_condition,
        download_data_condition,
    ]
    .into_iter()
    .flatten()
    {
        status.set_condition(cond);
    }

    let status_patch = serde_json::json!({
        "apiVersion": "servarr.dev/v1alpha1",
        "kind": "ServarrApp",
        "status": status,
    });

    let apps = Api::<ServarrApp>::namespaced(client.clone(), ns);
    apps.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(status_patch),
    )
    .await
    .map_err(Error::Kube)?;

    Ok(())
}

pub fn error_policy(app: Arc<ServarrApp>, error: &Error, ctx: Arc<Context>) -> Action {
    let app_type = app.spec.app.as_str();
    increment_reconcile_total(app_type, "error");
    warn!(error = %error.log_summary(), "reconciliation failed, requeuing");

    let recorder = Recorder::new(ctx.client.clone(), ctx.reporter.clone());
    let obj_ref = app.object_ref(&());
    // The Event note is tenant-visible (readable via `kubectl get events` in the app's
    // namespace), so it must go through the stricter public_summary(), not log_summary().
    let event_msg = error.public_summary();
    tokio::spawn(async move {
        let _ = recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "ReconcileError".into(),
                    note: Some(event_msg),
                    action: "Reconcile".into(),
                    secondary: None,
                },
                &obj_ref,
            )
            .await;
    });

    Action::requeue(Duration::from_secs(60))
}

/// Normalize a backup cron schedule to the 6-field form the `cron` crate
/// requires. The documented/standard format is 5-field (e.g. `0 3 * * *`), so a
/// `0` seconds field is prepended when the expression has 5 fields; 6- and
/// 7-field expressions pass through unchanged.
pub(crate) fn normalize_backup_schedule(expr: &str) -> String {
    let expr = expr.trim();
    if expr.split_whitespace().count() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

async fn maybe_run_backup(
    client: &Client,
    app: &ServarrApp,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Option<servarr_crds::BackupStatus> {
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let ns = ns.as_str();
    let backup_spec = app.spec.backup.as_ref()?;
    if !backup_spec.enabled || backup_spec.schedule.trim().is_empty() {
        return None;
    }

    let secret_name = app.spec.api_key_secret.as_deref()?;
    let api_key = match servarr_api::read_secret_key(client, ns, secret_name, "api-key").await {
        Ok(k) => k,
        Err(e) => {
            warn!(error = %e.log_summary(), "backup: failed to read API key");
            // status.backupStatus.lastBackupResult is tenant-visible, so it must go through
            // the stricter public_summary(), not log_summary().
            return Some(servarr_crds::BackupStatus {
                last_backup_result: Some(format!("secret read error: {}", e.public_summary())),
                ..Default::default()
            });
        }
    };

    // Only Servarr v3 apps support backup API
    if !matches!(
        app.spec.app,
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr
    ) {
        return None;
    }

    // Check if backup is due based on cron schedule.
    let schedule_expr = normalize_backup_schedule(&backup_spec.schedule);
    let schedule = match cron::Schedule::from_str(&schedule_expr) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, schedule = %backup_spec.schedule, "invalid cron schedule");
            let schedule_display = backup_spec.schedule.trim();
            if let Err(err) = recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "InvalidBackupSchedule".into(),
                        note: Some(format!(
                            "Invalid backup schedule '{}': {}",
                            schedule_display, e
                        )),
                        action: "Backup".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await
            {
                warn!(error = %err, "failed to publish InvalidBackupSchedule event");
            }
            return Some(servarr_crds::BackupStatus {
                last_backup_result: Some(format!("invalid schedule: {e}")),
                ..Default::default()
            });
        }
    };

    use chrono::Utc;
    let now = Utc::now();

    // Check last backup time from existing status
    let mut backup_time_corrupted = false;
    let last_backup = app
        .status
        .as_ref()
        .and_then(|s| s.backup_status.as_ref())
        .and_then(|bs| bs.last_backup_time.as_deref())
        .and_then(|t| match t.parse::<chrono::DateTime<Utc>>() {
            Ok(dt) => Some(dt),
            Err(e) => {
                warn!(last_backup_time = %t, error = %e, "failed to parse last_backup_time, treating as never backed up");
                backup_time_corrupted = true;
                None
            }
        });

    let is_due = match last_backup {
        Some(last) => schedule.after(&last).take(1).any(|next| next <= now),
        None => true, // Never backed up, do it now
    };

    if !is_due {
        // Return existing status unchanged
        return app.status.as_ref().and_then(|s| s.backup_status.clone());
    }

    let app_name = servarr_resources::common::service_name(app);
    let defaults = match servarr_crds::AppDefaults::for_app(&app.spec.app) {
        Ok(d) => d,
        Err(e) => {
            warn!(app = %app_name, error = %e, "failed to load app defaults; skipping backup");
            return None;
        }
    };
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    let app_kind = app_type_to_kind(&app.spec.app)?;
    // Safe as-is: `ServarrClient::new` builds the client but never sends a request, so it can
    // never return the response-body-derived `ApiResponse` variant; `{e}` here never echoes
    // external content.
    let api_client = match servarr_api::ServarrClient::new(&base_url, &api_key, app_kind) {
        Ok(c) => c,
        Err(e) => {
            return Some(servarr_crds::BackupStatus {
                last_backup_result: Some(format!("client error: {e}")),
                ..Default::default()
            });
        }
    };

    let app_type = app.spec.app.as_str();

    if backup_time_corrupted && let Err(err) = recorder
        .publish(
            &Event {
                type_: EventType::Warning,
                reason: "CorruptedBackupTime".into(),
                note: Some(
                    "Failed to parse last_backup_time; backup triggered due to unparseable timestamp".into(),
                ),
                action: "Backup".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await
    {
        warn!(error = %err, "failed to publish CorruptedBackupTime event");
    }

    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "BackupStarted".into(),
                note: Some("Scheduled backup started".into()),
                action: "Backup".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;

    info!(app = %app_name, "creating backup");
    match api_client.create_backup().await {
        Ok(backup) => {
            info!(app = %app_name, backup_id = backup.id, "backup created");
            increment_backup_operations(app_type, "backup", "success");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "BackupCompleted".into(),
                        note: Some(format!("Backup {} created successfully", backup.id)),
                        action: "Backup".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;

            // Prune old backups if over retention count
            let retention = backup_spec.retention_count;
            if let Ok(backups) = api_client.list_backups().await
                && backups.len() as u32 > retention
            {
                let mut sorted = backups;
                sorted.sort_by(|a, b| a.time.cmp(&b.time));
                let to_delete = sorted.len() - retention as usize;
                for old in sorted.iter().take(to_delete) {
                    if let Err(e) = api_client.delete_backup(old.id).await {
                        warn!(backup_id = old.id, error = %e.log_summary(), "failed to prune old backup");
                    }
                }
            }

            Some(servarr_crds::BackupStatus {
                last_backup_time: Some(chrono_now()),
                last_backup_result: Some("success".into()),
                backup_count: retention.min(
                    api_client
                        .list_backups()
                        .await
                        .map(|b| b.len() as u32)
                        .unwrap_or(0),
                ),
            })
        }
        Err(e) => {
            let summary = e.log_summary();
            warn!(app = %app_name, error = %summary, "backup failed");
            increment_backup_operations(app_type, "backup", "error");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "BackupFailed".into(),
                        note: Some(format!("Backup failed: {summary}")),
                        action: "Backup".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;
            Some(servarr_crds::BackupStatus {
                last_backup_time: last_backup.map(|_| chrono_now()),
                last_backup_result: Some(format!("error: {summary}")),
                backup_count: 0,
            })
        }
    }
}

/// Attempt a backup restore for `app`.
///
/// Returns `Ok(())` when the restore completed and the annotation was removed.
/// Returns `Err` when any step fails; scale-up is always attempted before returning
/// the error so the deployment is not left at zero replicas.
///
/// # Errors
///
/// Returns `Err` on scale-down failure, API key read failure, client creation failure,
/// restore API failure, or annotation removal failure (annotation removal failure in
/// particular is returned as an error so the caller can surface it as a status condition,
/// which prevents the silent re-trigger loop caused by the annotation remaining).
async fn maybe_restore_backup(
    client: &Client,
    app: &ServarrApp,
    restore_id: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), TenantSafeMessage> {
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let ns = ns.as_str();
    let name = app.name_any();
    let name = name.as_str();
    // Only Servarr v3 apps support backup/restore API
    if !matches!(
        app.spec.app,
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr
    ) {
        warn!(%name, app_type = ?app.spec.app, "restore-from annotation set on unsupported app type, ignoring");
        return Ok(());
    }

    let backup_id: i64 = restore_id.parse().map_err(|_| {
        TenantSafeMessage::new(format!(
            "invalid restore-from value {restore_id:?}: expected integer backup ID"
        ))
    })?;

    info!(%name, backup_id, "restore-from-backup triggered");

    let deploy_api = Api::<Deployment>::namespaced(client.clone(), ns);

    // Step 1: Scale deployment to 0
    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "RestoreStarted".into(),
                note: Some(format!("Scaling down for restore from backup {backup_id}")),
                action: "Restore".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;

    // Step 1: Scale deployment to 0 and wait for pods to terminate.
    // Captured as a Result so scale-up (Step 3) always runs even if this fails.
    let scale_down_outcome: Result<(), TenantSafeMessage> = async {
        let scale_down = serde_json::json!({
            "spec": { "replicas": 0 }
        });
        deploy_api
            .patch(name, &PatchParams::default(), &Patch::Merge(scale_down))
            .await
            .map_err(|e| {
                TenantSafeMessage::new(format!(
                    "failed to scale down for restore: {}",
                    kube_err_public_summary(&e)
                ))
            })?;

        // Wait for pods to terminate (poll for up to 60 seconds)
        for _ in 0..12 {
            tokio::time::sleep(Duration::from_secs(5)).await;
            match deploy_api.get(name).await {
                Ok(d) => {
                    let ready = d
                        .status
                        .as_ref()
                        .and_then(|s| s.ready_replicas)
                        .unwrap_or(0);
                    if ready == 0 {
                        break;
                    }
                }
                Err(e) => {
                    warn!(%name, error = %kube_err_summary(&e), "failed to check deployment status during restore");
                    break;
                }
            }
        }
        Ok(())
    }
    .await;

    // Step 2: Build API client and call restore; always attempt scale-up on failure.
    let restore_outcome = if scale_down_outcome.is_ok() {
        try_restore(client, app, backup_id, recorder, obj_ref).await
    } else {
        scale_down_outcome
    };

    // Step 3: Scale the deployment back up (always runs, even on restore failure).
    let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
    if let Err(se) = deploy_api
        .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
        .await
    {
        warn!(%name, error = %kube_err_summary(&se), "failed to scale back up after restore; deployment may be at zero replicas");
    }

    restore_outcome?;

    // Step 4: Remove the restore-from annotation to prevent re-triggering.
    // Return Err if removal fails so the status condition reflects the failure and
    // operators can diagnose the re-trigger loop.
    let servarr_api_resource = Api::<ServarrApp>::namespaced(client.clone(), ns);
    let remove_annotation = serde_json::json!({
        "metadata": {
            "annotations": {
                "servarr.dev/restore-from": null
            }
        }
    });
    servarr_api_resource
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(remove_annotation),
        )
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "restore succeeded but failed to remove annotation \
                 (will re-trigger on next reconcile): {}",
                kube_err_public_summary(&e)
            ))
        })?;

    Ok(())
}

/// Inner restore logic — performs the API call and fires events.
///
/// Separated from `maybe_restore_backup` so the outer function can unconditionally
/// attempt scale-up regardless of whether this returns `Ok` or `Err`.
async fn try_restore(
    client: &Client,
    app: &ServarrApp,
    backup_id: i64,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), TenantSafeMessage> {
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let ns = ns.as_str();
    let name = app.name_any();
    let name = name.as_str();
    let secret_name =
        app.spec.api_key_secret.as_deref().ok_or_else(|| {
            TenantSafeMessage::new("no api_key_secret configured, cannot restore")
        })?;
    let api_key = servarr_api::read_secret_key(client, ns, secret_name, "api-key")
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to read API key for restore: {}",
                e.public_summary()
            ))
        })?;

    let app_name = servarr_resources::common::service_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app)
        .map_err(|e| TenantSafeMessage::new(format!("failed to load app defaults: {e}")))?;
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    let Some(app_kind) = app_type_to_kind(&app.spec.app) else {
        return Err(TenantSafeMessage::new(format!(
            "restore: app type {:?} has no AppKind mapping",
            app.spec.app
        )));
    };

    // `ServarrClient::new` can fail with any `ApiError` variant (an invalid base URL, an
    // API-key charset rejection, a client-build transport error); `log_summary()` drops any
    // echoed input so the tenant-facing message never carries external content.
    let servarr_client =
        servarr_api::ServarrClient::new(&base_url, &api_key, app_kind).map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to create API client for restore: {}",
                e.log_summary()
            ))
        })?;

    match servarr_client.restore_backup(backup_id).await {
        Ok(()) => {
            info!(%name, backup_id, "restore completed successfully");
            increment_backup_operations(app.spec.app.as_str(), "restore", "success");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "RestoreComplete".into(),
                        note: Some(format!("Successfully restored from backup {backup_id}")),
                        action: "Restore".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;
            Ok(())
        }
        Err(e) => {
            let summary = e.log_summary();
            warn!(%name, backup_id, error = %summary, "restore API call failed");
            increment_backup_operations(app.spec.app.as_str(), "restore", "error");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "RestoreFailed".into(),
                        note: Some(format!(
                            "Failed to restore from backup {backup_id}: {summary}"
                        )),
                        action: "Restore".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;
            Err(TenantSafeMessage::new(format!(
                "restore API call failed: {summary}"
            )))
        }
    }
}

/// A discovered *arr app in the namespace with its service URL and API key.
#[derive(Debug)]
pub(crate) struct DiscoveredApp {
    pub(crate) name: String,
    pub(crate) app_type: AppType,
    /// Hostname component (e.g. `"sonarr.default.svc"`).
    pub(crate) host: String,
    /// Port component, matching the `i32` type used by `ServicePort.port`.
    pub(crate) port: i32,
    pub(crate) api_key: String,
    pub(crate) instance: Option<String>,
}

impl DiscoveredApp {
    /// Compute the base URL from host and port components.
    /// Issue #14: Compute on demand instead of storing redundantly.
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

/// Discover all Servarr v3 apps (Sonarr/Radarr/Lidarr) in a namespace
/// and resolve their service URLs and API keys.
pub(crate) async fn discover_namespace_apps(
    client: &Client,
    namespace: &str,
) -> Result<Vec<DiscoveredApp>, TenantSafeMessage> {
    use kube::api::ListParams;

    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = api.list(&ListParams::default()).await.map_err(|e| {
        TenantSafeMessage::new(format!(
            "failed to list ServarrApps: {}",
            kube_err_public_summary(&e)
        ))
    })?;

    let mut discovered = Vec::new();
    for app in &apps {
        // Discover Servarr apps and request coordinators that expose an
        // operator-managed API key. Plex is excluded: it uses plex.tv account
        // auth, so it has no api_key_secret and would always be skipped below.
        if !matches!(
            app.spec.app,
            AppType::Sonarr
                | AppType::Radarr
                | AppType::Lidarr
                | AppType::Seerr
                | AppType::Tautulli
        ) {
            continue;
        }

        let secret_name = match app.spec.api_key_secret.as_deref() {
            Some(s) => s,
            None => continue,
        };

        let api_key = match servarr_api::read_secret_key(client, namespace, secret_name, "api-key")
            .await
        {
            Ok(k) => k,
            Err(e) => {
                warn!(app = %app.name_any(), error = %e.log_summary(), "skipping app: failed to read API key");
                continue;
            }
        };

        let app_name = servarr_resources::common::service_name(app);
        let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app)
            .map_err(|e| TenantSafeMessage::new(format!("failed to load app defaults: {e}")))?;
        let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
        let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
        let host = format!("{app_name}.{namespace}.svc");

        discovered.push(DiscoveredApp {
            name: app.name_any(),
            app_type: app.spec.app.clone(),
            host,
            port,
            api_key,
            instance: app.spec.instance.clone(),
        });
    }

    Ok(discovered)
}

/// Sync discovered namespace apps into Prowlarr as registered applications.
async fn sync_prowlarr_apps(
    client: &Client,
    prowlarr: &ServarrApp,
    target_ns: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), TenantSafeMessage> {
    let prowlarr_name = prowlarr.name_any();
    let ns = prowlarr.namespace().unwrap_or_else(|| "default".into());

    // Build Prowlarr client
    let secret_name = prowlarr
        .spec
        .api_key_secret
        .as_deref()
        .ok_or_else(|| TenantSafeMessage::new("Prowlarr sync requires api_key_secret"))?;
    let prowlarr_key = servarr_api::read_secret_key(client, &ns, secret_name, "api-key")
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to read Prowlarr API key: {}",
                e.public_summary()
            ))
        })?;

    let prowlarr_app_name = servarr_resources::common::service_name(prowlarr);
    let defaults = servarr_crds::AppDefaults::for_app(&prowlarr.spec.app)
        .map_err(|e| TenantSafeMessage::new(format!("failed to load app defaults: {e}")))?;
    let svc_spec = prowlarr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let prowlarr_url = format!("http://{prowlarr_app_name}.{ns}.svc:{port}");

    let prowlarr_client =
        servarr_api::ProwlarrClient::new(&prowlarr_url, &prowlarr_key).map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to create Prowlarr client: {}",
                e.log_summary()
            ))
        })?;

    // Discover apps in target namespace
    let discovered = discover_namespace_apps(client, target_ns).await?;

    // Get current Prowlarr applications
    let existing = prowlarr_client.list_applications().await.map_err(|e| {
        TenantSafeMessage::new(format!(
            "failed to list Prowlarr applications: {}",
            e.log_summary()
        ))
    })?;

    // Build a map of existing apps by base URL for diffing
    let existing_by_url: std::collections::HashMap<String, &servarr_api::prowlarr::ProwlarrApp> =
        existing
            .iter()
            .filter_map(|a| {
                a.fields
                    .iter()
                    .find(|f| f.name == "baseUrl")
                    .and_then(|f| f.value.as_str())
                    .map(|url| (url.to_string(), a))
            })
            .collect();

    let auto_remove = prowlarr
        .spec
        .prowlarr_sync
        .as_ref()
        .map(|s| s.auto_remove)
        .unwrap_or(true);

    // Add or update discovered apps
    let mut synced_urls = std::collections::HashSet::new();
    for app in &discovered {
        synced_urls.insert(app.base_url());

        let implementation = match app.app_type {
            AppType::Sonarr => "Sonarr",
            AppType::Radarr => "Radarr",
            AppType::Lidarr => "Lidarr",
            _ => continue,
        };

        let config_contract = match app.app_type {
            AppType::Sonarr => "SonarrSettings",
            AppType::Radarr => "RadarrSettings",
            AppType::Lidarr => "LidarrSettings",
            _ => continue,
        };

        let new_app = servarr_api::prowlarr::ProwlarrApp {
            id: 0,
            name: app.name.clone(),
            sync_level: "fullSync".into(),
            implementation: implementation.into(),
            config_contract: config_contract.into(),
            fields: vec![
                servarr_api::prowlarr::ProwlarrAppField {
                    name: "baseUrl".into(),
                    value: serde_json::Value::String(app.base_url()),
                },
                servarr_api::prowlarr::ProwlarrAppField {
                    name: "apiKey".into(),
                    value: serde_json::Value::String(app.api_key.clone()),
                },
            ],
            tags: Vec::new(),
        };

        if let Some(existing_app) = existing_by_url.get(&app.base_url()) {
            // Update if name changed
            if existing_app.name != app.name {
                info!(prowlarr = %prowlarr_name, app = %app.name, "updating Prowlarr application");
                let mut updated = new_app;
                updated.id = existing_app.id;
                if let Err(e) = prowlarr_client
                    .update_application(existing_app.id, &updated)
                    .await
                {
                    let summary = e.log_summary();
                    warn!(
                        app = %app.name,
                        error = %summary,
                        "failed to update Prowlarr application"
                    );
                }
            }
        } else {
            // Add new
            info!(prowlarr = %prowlarr_name, app = %app.name, "adding application to Prowlarr");
            if let Err(e) = prowlarr_client.add_application(&new_app).await {
                let summary = e.log_summary();
                warn!(app = %app.name, error = %summary, "failed to add Prowlarr application");
            }
        }
    }

    // Remove stale apps (those in Prowlarr but not discovered)
    if auto_remove {
        for app in &existing {
            let url = app
                .fields
                .iter()
                .find(|f| f.name == "baseUrl")
                .and_then(|f| f.value.as_str())
                .unwrap_or("");
            if !url.is_empty() && !synced_urls.contains(url) {
                info!(prowlarr = %prowlarr_name, app = %app.name, "removing stale application from Prowlarr");
                if let Err(e) = prowlarr_client.delete_application(app.id).await {
                    warn!(app = %app.name, error = %e.log_summary(), "failed to remove Prowlarr application");
                }
            }
        }
    }

    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "ProwlarrSyncComplete".into(),
                note: Some(format!("Synced {} apps to Prowlarr", discovered.len())),
                action: "ProwlarrSync".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;

    Ok(())
}

/// Check if any Prowlarr instance with prowlarr_sync.enabled exists in the namespace.
async fn prowlarr_sync_exists(client: &Client, namespace: &str) -> bool {
    use kube::api::ListParams;
    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    match api.list(&ListParams::default()).await {
        Ok(list) => list.iter().any(|a| {
            a.spec.app == AppType::Prowlarr
                && a.spec.prowlarr_sync.as_ref().is_some_and(|s| s.enabled)
        }),
        Err(e) => {
            warn!(error = %kube_err_summary(&e), %namespace, "failed to list ServarrApps for prowlarr-sync check, assuming no sync exists");
            false
        }
    }
}

/// Whether a cleanup failure proves the downstream target is already gone (`Terminal` — safe to
/// treat as idempotent success, since retrying can never make an absent target more absent) or
/// might still succeed on a later attempt (`Transient` — must keep the finalizer so the cleanup
/// is retried, never silently dropped). See #451.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupSeverity {
    Terminal,
    Transient,
}

/// Classifies an error's [`CleanupSeverity`]. Implemented only for the concrete error types the
/// cleanup path actually produces (`kube::Error`, `SecretError`, `ApiError`) — deliberately no
/// blanket/default impl, so a new error type flowing through [`CleanupMapErr`] must get an
/// explicit, reviewed classification rather than silently defaulting to one severity or the other.
trait ClassifyCleanupSeverity {
    fn cleanup_severity(&self) -> CleanupSeverity;
}

impl ClassifyCleanupSeverity for kube::Error {
    fn cleanup_severity(&self) -> CleanupSeverity {
        match self {
            // The API server has no such object (Secret, ServarrApp, ...) — provably absent.
            kube::Error::Api(status) if status.code == 404 => CleanupSeverity::Terminal,
            _ => CleanupSeverity::Transient,
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
async fn cleanup_prowlarr_registration(
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
/// the log-only sanitizer variant (`log_summary()` / `kube_err_summary()`) for `error`, and route
/// the same error through `TenantSafeMessage` for `tenant_msg`.
#[derive(Debug)]
struct CleanupFailure {
    error: anyhow::Error,
    tenant_msg: TenantSafeMessage,
    severity: CleanupSeverity,
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
async fn cleanup_prowlarr_registration_body(
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

/// Sync discovered Sonarr/Radarr apps into Seerr as registered servers.
async fn sync_seerr_servers(
    client: &Client,
    seerr: &ServarrApp,
    target_ns: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), TenantSafeMessage> {
    let seerr_name = seerr.name_any();
    let ns = seerr.namespace().unwrap_or_else(|| "default".into());

    // Build Seerr client
    let secret_name = seerr
        .spec
        .api_key_secret
        .as_deref()
        .ok_or_else(|| TenantSafeMessage::new("Seerr sync requires api_key_secret"))?;
    let seerr_key = servarr_api::read_secret_key(client, &ns, secret_name, "api-key")
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to read Seerr API key: {}",
                e.public_summary()
            ))
        })?;

    let seerr_app_name = servarr_resources::common::service_name(seerr);
    let defaults = servarr_crds::AppDefaults::for_app(&seerr.spec.app)
        .map_err(|e| TenantSafeMessage::new(format!("failed to load app defaults: {e}")))?;
    let svc_spec = seerr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let seerr_url = format!("http://{seerr_app_name}.{ns}.svc:{port}");

    let seerr_client = servarr_api::SeerrClient::new(&seerr_url, &seerr_key);

    // Discover Sonarr/Radarr apps in target namespace
    let discovered = discover_namespace_apps(client, target_ns).await?;

    // Get existing server registrations
    let existing_sonarr = seerr_client.list_sonarr().await.map_err(|e| {
        TenantSafeMessage::new(format!(
            "failed to list Seerr Sonarr servers: {}",
            e.log_summary()
        ))
    })?;
    let existing_radarr = seerr_client.list_radarr().await.map_err(|e| {
        TenantSafeMessage::new(format!(
            "failed to list Seerr Radarr servers: {}",
            e.log_summary()
        ))
    })?;

    // Get Seerr config for default profile/directory settings
    let seerr_config = match &seerr.spec.app_config {
        Some(servarr_crds::AppConfig::Seerr(c)) => Some(c.as_ref()),
        _ => None,
    };

    let auto_remove = seerr
        .spec
        .seerr_sync
        .as_ref()
        .map(|s| s.auto_remove)
        .unwrap_or(true);

    // Track which hostname:port combos we sync so we can detect stale entries
    let mut synced_sonarr_keys: std::collections::HashSet<(String, i32)> =
        std::collections::HashSet::new();
    let mut synced_radarr_keys: std::collections::HashSet<(String, i32)> =
        std::collections::HashSet::new();

    for app in &discovered {
        let hostname = app.host.clone();
        let port = f64::from(app.port); // Seerr API uses f64 for port numbers
        let is4k = app.instance.as_deref() == Some("4k");

        match app.app_type {
            AppType::Sonarr => {
                let key = (hostname.clone(), app.port);
                synced_sonarr_keys.insert(key);

                let sonarr_defaults = seerr_config.and_then(|c| c.sonarr.as_ref());
                let (profile_id, profile_name, root_folder, enable_season_folders) = if is4k {
                    let four_k = sonarr_defaults.and_then(|d| d.four_k.as_ref());
                    (
                        four_k.map(|f| f.profile_id).unwrap_or(0.0),
                        four_k.map(|f| f.profile_name.clone()).unwrap_or_default(),
                        four_k.map(|f| f.root_folder.clone()).unwrap_or_default(),
                        four_k.and_then(|f| f.enable_season_folders).unwrap_or(true),
                    )
                } else {
                    (
                        sonarr_defaults.map(|d| d.profile_id).unwrap_or(0.0),
                        sonarr_defaults
                            .map(|d| d.profile_name.clone())
                            .unwrap_or_default(),
                        sonarr_defaults
                            .map(|d| d.root_folder.clone())
                            .unwrap_or_default(),
                        sonarr_defaults
                            .and_then(|d| d.enable_season_folders)
                            .unwrap_or(true),
                    )
                };

                let settings = overseerr::models::SonarrSettings::new(
                    app.name.clone(),
                    hostname.clone(),
                    port,
                    app.api_key.clone(),
                    false,
                    profile_id,
                    profile_name,
                    root_folder,
                    is4k,
                    enable_season_folders,
                    !is4k,
                );

                // Match existing by hostname + port
                if let Some(existing) = existing_sonarr
                    .iter()
                    .find(|s| s.hostname == hostname && s.port == port)
                {
                    let id = existing.id.unwrap_or(0.0) as i32;
                    let mut updated = settings;
                    updated.id = existing.id;
                    if let Err(e) = seerr_client.update_sonarr(id, updated).await {
                        warn!(app = %app.name, error = %e.log_summary(), "failed to update Sonarr in Seerr");
                    }
                } else {
                    info!(seerr = %seerr_name, app = %app.name, "adding Sonarr server to Seerr");
                    if let Err(e) = seerr_client.create_sonarr(settings).await {
                        warn!(app = %app.name, error = %e.log_summary(), "failed to add Sonarr to Seerr");
                    }
                }
            }
            AppType::Radarr => {
                let key = (hostname.clone(), app.port);
                synced_radarr_keys.insert(key);

                let radarr_defaults = seerr_config.and_then(|c| c.radarr.as_ref());
                let (profile_id, profile_name, root_folder, minimum_availability) = if is4k {
                    let four_k = radarr_defaults.and_then(|d| d.four_k.as_ref());
                    (
                        four_k.map(|f| f.profile_id).unwrap_or(0.0),
                        four_k.map(|f| f.profile_name.clone()).unwrap_or_default(),
                        four_k.map(|f| f.root_folder.clone()).unwrap_or_default(),
                        four_k
                            .and_then(|f| f.minimum_availability.clone())
                            .unwrap_or_else(|| "released".to_string()),
                    )
                } else {
                    (
                        radarr_defaults.map(|d| d.profile_id).unwrap_or(0.0),
                        radarr_defaults
                            .map(|d| d.profile_name.clone())
                            .unwrap_or_default(),
                        radarr_defaults
                            .map(|d| d.root_folder.clone())
                            .unwrap_or_default(),
                        radarr_defaults
                            .and_then(|d| d.minimum_availability.clone())
                            .unwrap_or_else(|| "released".to_string()),
                    )
                };

                let settings = overseerr::models::RadarrSettings::new(
                    app.name.clone(),
                    hostname.clone(),
                    port,
                    app.api_key.clone(),
                    false,
                    profile_id,
                    profile_name,
                    root_folder,
                    is4k,
                    minimum_availability,
                    !is4k,
                );

                // Match existing by hostname + port
                if let Some(existing) = existing_radarr
                    .iter()
                    .find(|s| s.hostname == hostname && s.port == port)
                {
                    let id = existing.id.unwrap_or(0.0) as i32;
                    let mut updated = settings;
                    updated.id = existing.id;
                    if let Err(e) = seerr_client.update_radarr(id, updated).await {
                        warn!(app = %app.name, error = %e.log_summary(), "failed to update Radarr in Seerr");
                    }
                } else {
                    info!(seerr = %seerr_name, app = %app.name, "adding Radarr server to Seerr");
                    if let Err(e) = seerr_client.create_radarr(settings).await {
                        warn!(app = %app.name, error = %e.log_summary(), "failed to add Radarr to Seerr");
                    }
                }
            }
            _ => continue,
        }
    }

    // Remove stale servers
    if auto_remove {
        for existing in &existing_sonarr {
            let key = (existing.hostname.clone(), existing.port as i32);
            if !synced_sonarr_keys.contains(&key) {
                let id = existing.id.unwrap_or(0.0) as i32;
                info!(seerr = %seerr_name, server = %existing.name, "removing stale Sonarr server from Seerr");
                if let Err(e) = seerr_client.delete_sonarr(id).await {
                    warn!(server = %existing.name, error = %e.log_summary(), "failed to remove stale Sonarr from Seerr");
                }
            }
        }
        for existing in &existing_radarr {
            let key = (existing.hostname.clone(), existing.port as i32);
            if !synced_radarr_keys.contains(&key) {
                let id = existing.id.unwrap_or(0.0) as i32;
                info!(seerr = %seerr_name, server = %existing.name, "removing stale Radarr server from Seerr");
                if let Err(e) = seerr_client.delete_radarr(id).await {
                    warn!(server = %existing.name, error = %e.log_summary(), "failed to remove stale Radarr from Seerr");
                }
            }
        }
    }

    let sonarr_count = discovered
        .iter()
        .filter(|a| a.app_type == AppType::Sonarr)
        .count();
    let radarr_count = discovered
        .iter()
        .filter(|a| a.app_type == AppType::Radarr)
        .count();
    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "SeerrSyncComplete".into(),
                note: Some(format!(
                    "Synced {sonarr_count} Sonarr + {radarr_count} Radarr servers to Seerr"
                )),
                action: "SeerrSync".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;

    Ok(())
}

/// Sync Bazarr's Sonarr/Radarr integration via POST /api/system/settings.
///
/// Called on every reconcile when `bazarr_sync.enabled` is true.
async fn sync_bazarr_apps(
    client: &Client,
    bazarr: &ServarrApp,
    target_ns: &str,
) -> Result<(), TenantSafeMessage> {
    let bazarr_name = bazarr.name_any();
    let ns = bazarr.namespace().unwrap_or_else(|| "default".into());

    // Read Bazarr's operator-managed API key
    let api_key_secret = servarr_resources::common::child_name(bazarr, "api-key");
    let bazarr_key = servarr_api::read_secret_key(client, &ns, &api_key_secret, "api-key")
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to read Bazarr API key: {}",
                e.public_summary()
            ))
        })?;

    let bazarr_app_name = servarr_resources::common::service_name(bazarr);
    let defaults = servarr_crds::AppDefaults::for_app(&bazarr.spec.app)
        .map_err(|e| TenantSafeMessage::new(format!("failed to load app defaults: {e}")))?;
    let svc_spec = bazarr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let bazarr_url = format!("http://{bazarr_app_name}.{ns}.svc:{port}");

    let bazarr_client = servarr_api::BazarrClient::new(&bazarr_url, &bazarr_key).map_err(|e| {
        TenantSafeMessage::new(format!(
            "failed to create Bazarr client: {}",
            e.log_summary()
        ))
    })?;

    let auto_remove = bazarr
        .spec
        .bazarr_sync
        .as_ref()
        .map(|s| s.auto_remove)
        .unwrap_or(true);

    // Discover Sonarr and Radarr apps in the target namespace
    let discovered = discover_namespace_apps(client, target_ns).await?;

    let has_sonarr = discovered.iter().any(|a| a.app_type == AppType::Sonarr);
    let has_radarr = discovered.iter().any(|a| a.app_type == AppType::Radarr);

    let mut first_error: Option<TenantSafeMessage> = None;

    for app in &discovered {
        match app.app_type {
            AppType::Sonarr => {
                info!(bazarr = %bazarr_name, sonarr = %app.name, "syncing Sonarr into Bazarr");
                if let Err(e) = bazarr_client
                    .configure_sonarr(&app.host, app.port, &app.api_key)
                    .await
                {
                    let summary = e.log_summary();
                    warn!(bazarr = %bazarr_name, sonarr = %app.name, error = %summary,
                        "failed to configure Sonarr in Bazarr");
                    first_error.get_or_insert_with(|| {
                        TenantSafeMessage::new(format!(
                            "configure_sonarr({}) failed: {summary}",
                            app.name
                        ))
                    });
                }
            }
            AppType::Radarr => {
                info!(bazarr = %bazarr_name, radarr = %app.name, "syncing Radarr into Bazarr");
                if let Err(e) = bazarr_client
                    .configure_radarr(&app.host, app.port, &app.api_key)
                    .await
                {
                    let summary = e.log_summary();
                    warn!(bazarr = %bazarr_name, radarr = %app.name, error = %summary,
                        "failed to configure Radarr in Bazarr");
                    first_error.get_or_insert_with(|| {
                        TenantSafeMessage::new(format!(
                            "configure_radarr({}) failed: {summary}",
                            app.name
                        ))
                    });
                }
            }
            _ => {}
        }
    }

    if auto_remove {
        if !has_sonarr && let Err(e) = bazarr_client.disable_sonarr().await {
            let summary = e.log_summary();
            warn!(bazarr = %bazarr_name, error = %summary, "failed to disable Sonarr in Bazarr");
            first_error.get_or_insert_with(|| {
                TenantSafeMessage::new(format!("disable_sonarr failed: {summary}"))
            });
        }
        if !has_radarr && let Err(e) = bazarr_client.disable_radarr().await {
            let summary = e.log_summary();
            warn!(bazarr = %bazarr_name, error = %summary, "failed to disable Radarr in Bazarr");
            first_error.get_or_insert_with(|| {
                TenantSafeMessage::new(format!("disable_radarr failed: {summary}"))
            });
        }
    }

    if let Some(e) = first_error {
        return Err(e);
    }

    Ok(())
}

/// Sync Sonarr, Radarr, Seerr, Tautulli, and Plex into Maintainerr.
///
/// Called on every reconcile when `maintainerr_sync.enabled` is true. Discovers
/// Sonarr, Radarr, Seerr, and Tautulli instances in the target namespace and
/// registers them with Maintainerr. split4k Sonarr/Radarr instances are discovered
/// as separate `ServarrApp`s, so each is registered independently. Plex is looked up
/// separately (it has no `api_key_secret` so `discover_namespace_apps` excludes it)
/// and synced using the plex.tv auth token from `plexTokenSecret`.
///
/// Registration is idempotent: existing Sonarr/Radarr servers are listed first and
/// already-registered names are skipped, so repeated reconciles do not accumulate
/// duplicate entries.
///
/// Per-app failures are logged and do not abort the loop, but the function returns
/// `Err` if any registration failed so the `MaintainerrSyncReady` status condition
/// reflects the partial failure. The caller converts this into a condition rather
/// than propagating it, so a sync failure never blocks the rest of reconciliation.
async fn sync_maintainerr_servers(
    client: &Client,
    maintainerr: &ServarrApp,
    target_ns: &str,
    base_url_override: Option<&str>,
) -> Result<(), TenantSafeMessage> {
    let maintainerr_name = maintainerr.name_any();
    let ns = maintainerr.namespace().unwrap_or_else(|| "default".into());

    // Read Maintainerr's operator-managed API key
    let api_key_secret = servarr_resources::common::child_name(maintainerr, "api-key");
    let maintainerr_key = servarr_api::read_secret_key(client, &ns, &api_key_secret, "api-key")
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to read Maintainerr API key: {}",
                e.public_summary()
            ))
        })?;

    let maintainerr_app_name = servarr_resources::common::service_name(maintainerr);
    let defaults = servarr_crds::AppDefaults::for_app(&maintainerr.spec.app)
        .map_err(|e| TenantSafeMessage::new(format!("failed to load app defaults: {e}")))?;
    let svc_spec = maintainerr
        .spec
        .service
        .as_ref()
        .unwrap_or(&defaults.service);
    // A Maintainerr service with no port is a malformed spec/defaults — surface it
    // rather than silently constructing a URL against the wrong port.
    let port = svc_spec.ports.first().map(|p| p.port).ok_or_else(|| {
        TenantSafeMessage::new(
            "Maintainerr service spec has no ports; check spec.service or app defaults",
        )
    })?;
    let maintainerr_url = base_url_override
        .map(str::to_owned)
        .unwrap_or_else(|| format!("http://{maintainerr_app_name}.{ns}.svc:{port}"));

    let maintainerr_client =
        servarr_api::MaintainerrClient::new(&maintainerr_url, &maintainerr_key).map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to create Maintainerr client: {}",
                e.log_summary()
            ))
        })?;

    let mut failures = 0;

    // Read Plex token if configured
    let mut plex_token = None;
    if let Some(sync_spec) = &maintainerr.spec.maintainerr_sync
        && let Some(secret_name) = &sync_spec.plex_token_secret
    {
        match servarr_api::read_secret_key(client, &ns, secret_name, "plex-token").await {
            Ok(token) => plex_token = Some(token),
            // 404 = secret not found, intentional-skip case when Plex is optional
            Err(servarr_api::SecretError::Kube(kube::Error::Api(ref api_err)))
                if api_err.code == 404 =>
            {
                debug!(
                    maintainerr = %maintainerr_name,
                    secret = %secret_name,
                    namespace = %ns,
                    "Plex token secret not found; Plex will not be configured"
                );
            }
            // Any other Kube error (permission denied, timeout, connection failure, etc.)
            // is an infrastructure failure and should trigger backoff.
            Err(servarr_api::SecretError::Kube(e)) => {
                warn!(
                    maintainerr = %maintainerr_name,
                    secret = %secret_name,
                    namespace = %ns,
                    error = %kube_err_summary(&e),
                    "failed to read Plex token secret due to Kubernetes API error"
                );
                failures += 1;
            }
            // Non-Kube errors (missing key in secret, invalid UTF-8) are a data/config
            // problem, not an infra failure — retrying won't fix a missing key.
            Err(e) => {
                warn!(
                    maintainerr = %maintainerr_name,
                    secret = %secret_name,
                    namespace = %ns,
                    error = %e,
                    "failed to read Plex token secret; Plex will not be configured"
                );
            }
        }
    }

    // Discover apps in the target namespace (excludes Plex since it uses plex.tv auth, not api_key_secret)
    let discovered = discover_namespace_apps(client, target_ns).await?;

    let mut sonarr_count = 0;
    let mut radarr_count = 0;
    let mut seerr_configured = false;
    let mut tautulli_configured = false;
    let mut plex_configured = false;

    // Lookup Plex separately (discover_namespace_apps filters it out due to missing api_key_secret)
    let plex_app = if plex_token.is_some() {
        let all_apps = Api::<ServarrApp>::namespaced(client.clone(), target_ns);
        match all_apps.list(&kube::api::ListParams::default()).await {
            Ok(list) => list
                .items
                .into_iter()
                .find(|app| app.spec.app == AppType::Plex),
            Err(e) => {
                warn!(maintainerr = %maintainerr_name, error = %kube_err_summary(&e),
                    "failed to list apps in target namespace; Plex will not be configured");
                failures += 1;
                None
            }
        }
    } else {
        None
    };

    // List already-registered servers so re-registration is idempotent (#132).
    // On API error, propagate immediately to trigger controller retry with backoff (#199).
    // Do not fall back to empty set: that causes duplicate registrations on every transient
    // failure, with no reconcile error to trigger backoff.
    let existing_sonarr: std::collections::HashSet<String> = maintainerr_client
        .list_sonarr()
        .await
        .map_err(|e| {
            let summary = e.log_summary();
            error!(maintainerr = %maintainerr_name, error = %summary,
                "failed to list existing Sonarr servers from Maintainerr; aborting sync to prevent duplicates");
            TenantSafeMessage::new(format!("list_sonarr failed: {summary}"))
        })?
        .into_iter()
        .map(|s| s.name)
        .collect();
    let existing_radarr: std::collections::HashSet<String> = maintainerr_client
        .list_radarr()
        .await
        .map_err(|e| {
            let summary = e.log_summary();
            error!(maintainerr = %maintainerr_name, error = %summary,
                "failed to list existing Radarr servers from Maintainerr; aborting sync to prevent duplicates");
            TenantSafeMessage::new(format!("list_radarr failed: {summary}"))
        })?
        .into_iter()
        .map(|s| s.name)
        .collect();

    // Register all discovered apps. split4k instances appear as separate apps in discovery.
    for app in &discovered {
        match app.app_type {
            AppType::Sonarr => {
                if existing_sonarr.contains(&app.name) {
                    continue;
                }
                info!(maintainerr = %maintainerr_name, sonarr = %app.name, "syncing Sonarr into Maintainerr");
                if let Err(e) = maintainerr_client
                    .add_sonarr(&app.name, &app.base_url(), &app.api_key)
                    .await
                {
                    let error_summary = e.log_summary();
                    warn!(maintainerr = %maintainerr_name, sonarr = %app.name, error = %error_summary,
                        "failed to sync Sonarr to Maintainerr");
                    failures += 1;
                } else {
                    sonarr_count += 1;
                }
            }
            AppType::Radarr => {
                if existing_radarr.contains(&app.name) {
                    continue;
                }
                info!(maintainerr = %maintainerr_name, radarr = %app.name, "syncing Radarr into Maintainerr");
                if let Err(e) = maintainerr_client
                    .add_radarr(&app.name, &app.base_url(), &app.api_key)
                    .await
                {
                    let error_summary = e.log_summary();
                    warn!(maintainerr = %maintainerr_name, radarr = %app.name, error = %error_summary,
                        "failed to sync Radarr to Maintainerr");
                    failures += 1;
                } else {
                    radarr_count += 1;
                }
            }
            AppType::Seerr if !seerr_configured => {
                info!(maintainerr = %maintainerr_name, seerr = %app.name, "syncing Seerr into Maintainerr");
                match maintainerr_client
                    .set_seerr(&app.base_url(), &app.api_key)
                    .await
                {
                    Ok(()) => seerr_configured = true,
                    Err(e) => {
                        let error_summary = e.log_summary();
                        warn!(maintainerr = %maintainerr_name, seerr = %app.name, error = %error_summary,
                            "failed to sync Seerr to Maintainerr");
                        failures += 1;
                    }
                }
            }
            AppType::Tautulli if !tautulli_configured => {
                info!(maintainerr = %maintainerr_name, tautulli = %app.name, "syncing Tautulli into Maintainerr");
                match maintainerr_client
                    .set_tautulli(&app.base_url(), &app.api_key)
                    .await
                {
                    Ok(()) => tautulli_configured = true,
                    Err(e) => {
                        let error_summary = e.log_summary();
                        warn!(maintainerr = %maintainerr_name, tautulli = %app.name, error = %error_summary,
                            "failed to sync Tautulli to Maintainerr");
                        failures += 1;
                    }
                }
            }
            _ => {
                // Unhandled app type: duplicate Seerr/Tautulli, future variants, or unsupported types.
                // Log at debug level so the operator can see what was skipped (important for troubleshooting
                // missing Seerr/Tautulli instances beyond the first).
                debug!(maintainerr = %maintainerr_name, app = %app.name, app_type = ?app.app_type,
                    "skipping app type in Maintainerr sync");
            }
        }
    }

    // Sync Plex if discovered and token is available (Plex lookup done separately above).
    // No `plex_configured` guard needed: this is the only place Plex is configured.
    if let (Some(plex), Some(token)) = (&plex_app, &plex_token) {
        let plex_name = plex.name_any();
        let plex_ns = plex.namespace().unwrap_or_else(|| "default".into());
        let (plex_defaults, plex_defaults_failed) =
            match servarr_crds::AppDefaults::for_app(&plex.spec.app) {
                Ok(defaults) => (defaults.service, false),
                Err(e) => {
                    warn!(maintainerr = %maintainerr_name, plex = %plex_name, error = %e,
                        "failed to load app defaults for Plex");
                    failures += 1;
                    (servarr_crds::ServiceSpec::default(), true)
                }
            };
        let plex_svc_spec = plex.spec.service.as_ref().unwrap_or(&plex_defaults);
        if let Some(plex_port) = plex_svc_spec.ports.first().map(|p| p.port as u16) {
            let plex_svc_name = servarr_resources::common::service_name(plex);
            let plex_host = format!("{plex_svc_name}.{plex_ns}.svc");

            info!(maintainerr = %maintainerr_name, plex = %plex_name, "syncing Plex into Maintainerr");
            // Token must be set before hostname/port: Maintainerr rejects Plex server
            // settings until an auth token is present (#156).
            if let Err(e) = maintainerr_client.set_plex_token(token).await {
                let error_summary = e.log_summary();
                warn!(maintainerr = %maintainerr_name, plex = %plex_name, error = %error_summary,
                    "failed to set Plex token in Maintainerr");
                failures += 1;
            } else if let Err(e) = maintainerr_client.set_plex(&plex_host, plex_port).await {
                warn!(maintainerr = %maintainerr_name, plex = %plex_name, error = %e.log_summary(),
                    "failed to set Plex hostname/port in Maintainerr");
                failures += 1;
            } else {
                plex_configured = true;
            }
        } else {
            warn!(maintainerr = %maintainerr_name, plex = %plex_name,
                "Plex service spec has no ports; cannot sync to Maintainerr");
            // Don't double-count: a defaults-load failure already incremented `failures`
            // and is the root cause of these empty ports.
            if !plex_defaults_failed {
                failures += 1;
            }
        }
    }

    info!(maintainerr = %maintainerr_name, sonarr_count, radarr_count,
        seerr_configured, tautulli_configured, plex_configured, failures,
        "Maintainerr sync complete");

    if failures > 0 {
        return Err(TenantSafeMessage::new(format!(
            "{failures} app(s) failed to sync into Maintainerr (see warnings above)"
        )));
    }

    Ok(())
}

/// Patch Jellyfin env vars onto the Subgen Deployment.
///
/// Called on every reconcile when `subgen_sync.enabled` is true.
async fn sync_subgen_jellyfin(
    client: &Client,
    subgen: &ServarrApp,
    target_ns: &str,
) -> Result<(), TenantSafeMessage> {
    let subgen_name = subgen.name_any();
    let ns = subgen.namespace().unwrap_or_else(|| "default".into());

    // Find Jellyfin in target namespace
    let all_apps = Api::<ServarrApp>::namespaced(client.clone(), target_ns);
    let app_list = all_apps
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to list ServarrApps: {}",
                kube_err_public_summary(&e)
            ))
        })?;

    let jellyfin = match app_list
        .items
        .iter()
        .find(|a| a.spec.app == AppType::Jellyfin)
    {
        Some(j) => j,
        None => {
            warn!(subgen = %subgen_name,
                "subgen-sync: no Jellyfin CR found in namespace {target_ns}, skipping");
            return Err(TenantSafeMessage::new(format!(
                "no Jellyfin CR found in namespace {target_ns}"
            )));
        }
    };

    // Verify Jellyfin's API key secret is accessible (fail fast before patching Deployment).
    let jf_secret_name = match jellyfin.spec.api_key_secret.as_deref() {
        Some(s) => s.to_string(),
        None => {
            warn!(subgen = %subgen_name,
                "subgen-sync: Jellyfin CR has no apiKeySecret, skipping");
            return Err(TenantSafeMessage::new(
                "Jellyfin CR has no apiKeySecret configured",
            ));
        }
    };
    // Verify the secret is readable; the Deployment will reference it via secretKeyRef.
    servarr_api::read_secret_key(client, target_ns, &jf_secret_name, "api-key")
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "Jellyfin API key secret {jf_secret_name} unreadable: {}",
                e.public_summary()
            ))
        })?;

    let jf_app_name = servarr_resources::common::service_name(jellyfin);
    let jf_defaults = servarr_crds::AppDefaults::for_app(&jellyfin.spec.app)
        .map_err(|e| TenantSafeMessage::new(format!("failed to load app defaults: {e}")))?;
    let jf_svc_spec = jellyfin
        .spec
        .service
        .as_ref()
        .unwrap_or(&jf_defaults.service);
    let jf_port = jf_svc_spec.ports.first().map(|p| p.port).unwrap_or(8096);
    let jf_url = format!("http://{jf_app_name}.{target_ns}.svc:{jf_port}");

    // Patch the env vars onto the Subgen Deployment via SSA.
    // JELLYFIN_TOKEN uses secretKeyRef so the token is never stored plaintext in the Deployment.
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), &ns);
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    let patch = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": &subgen_name },
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name": subgen.spec.app.as_str(),
                        "env": [
                            { "name": "JELLYFIN_SERVER", "value": jf_url },
                            {
                                "name": "JELLYFIN_TOKEN",
                                "valueFrom": {
                                    "secretKeyRef": {
                                        "name": &jf_secret_name,
                                        "key": "api-key"
                                    }
                                }
                            },
                        ]
                    }]
                }
            }
        }
    });

    deploy_api
        .patch(&subgen_name, &pp, &Patch::Apply(patch))
        .await
        .map_err(|e| {
            TenantSafeMessage::new(format!(
                "failed to patch Subgen Deployment: {}",
                kube_err_public_summary(&e)
            ))
        })?;

    info!(subgen = %subgen_name, jellyfin = %jf_app_name, "subgen-sync: injected Jellyfin env vars");
    Ok(())
}

/// Check if any Seerr instance with seerr_sync.enabled exists in the namespace.
async fn seerr_sync_exists(client: &Client, namespace: &str) -> bool {
    use kube::api::ListParams;
    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    match api.list(&ListParams::default()).await {
        Ok(list) => list.iter().any(|a| {
            a.spec.app == AppType::Seerr && a.spec.seerr_sync.as_ref().is_some_and(|s| s.enabled)
        }),
        Err(e) => {
            warn!(error = %kube_err_summary(&e), %namespace, "failed to list ServarrApps for seerr-sync check, assuming no sync exists");
            false
        }
    }
}

/// Remove this app's registration from Seerr when the CR is deleted.
///
/// See [`finish_cleanup`] for how the `Terminal`/`Transient` outcome of the cleanup body maps to
/// this function's return value and Event publication.
async fn cleanup_seerr_registration(
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
trait SeerrServerSettings {
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
trait SeerrAppKind {
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

struct SonarrSeerr;

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
async fn seerr_remove_server<K>(
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
async fn cleanup_seerr_registration_body(
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

fn chrono_now() -> String {
    // ISO 8601 timestamp with seconds precision
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Return true if `v` is a Kubernetes zero/default value that the API server
/// omits when serialising resources (false, 0, "", null).  A field absent from
/// `actual` but present as a zero value in `desired` is not real drift.
fn is_zero_value(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Bool(false) | serde_json::Value::Null => true,
        serde_json::Value::Number(n) => n.as_i64() == Some(0) || n.as_f64() == Some(0.0),
        serde_json::Value::String(s) => s.is_empty(),
        _ => false,
    }
}

/// Return paths where `desired` differs from `actual` for debugging drift.
fn json_diff_paths(
    desired: &serde_json::Value,
    actual: &serde_json::Value,
    path: String,
) -> Vec<String> {
    use serde_json::Value;
    match (desired, actual) {
        (Value::Object(d), Value::Object(a)) => d
            .iter()
            .flat_map(|(k, dv)| {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match a.get(k) {
                    Some(av) => json_diff_paths(dv, av, p),
                    // Kubernetes omits zero-value fields; treat as non-diff.
                    None if is_zero_value(dv) => vec![],
                    None => vec![format!("{p}: missing in actual")],
                }
            })
            .collect(),
        (Value::Array(d), Value::Array(a)) if d.len() == a.len() => d
            .iter()
            .zip(a.iter())
            .enumerate()
            .flat_map(|(i, (dv, av))| json_diff_paths(dv, av, format!("{path}[{i}]")))
            .collect(),
        (Value::Array(d), Value::Array(a)) => {
            vec![format!("{path}: array length {0} vs {1}", d.len(), a.len())]
        }
        _ if desired == actual => vec![],
        _ => vec![format!("{path}: {desired} vs {actual}")],
    }
}

/// Check that every field in `desired` exists with the same value in `actual`.
/// Extra fields in `actual` (e.g. Kubernetes defaults) are ignored.
/// Fields absent from `actual` but present as zero values in `desired` are
/// not considered drift — Kubernetes omits zero-value fields on read.
fn json_is_subset(desired: &serde_json::Value, actual: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (desired, actual) {
        (Value::Object(d), Value::Object(a)) => d.iter().all(|(k, dv)| match a.get(k) {
            Some(av) => json_is_subset(dv, av),
            None => is_zero_value(dv),
        }),
        (Value::Array(d), Value::Array(a)) => {
            d.len() == a.len()
                && d.iter()
                    .zip(a.iter())
                    .all(|(dv, av)| json_is_subset(dv, av))
        }
        // Leaf values: exact match
        _ => desired == actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ---- Error::log_summary ----

    #[test]
    fn error_log_summary_kube_variant_drops_message_keeps_status_code() {
        let status = kube::core::Status {
            code: 403,
            message: "secrets \"super-secret-name\" is forbidden: User cannot get".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        };
        let err = Error::Kube(kube::Error::Api(Box::new(status)));
        let summary = err.log_summary();
        assert!(
            summary.contains("403"),
            "summary should keep the status code: {summary}"
        );
        assert!(
            !summary.contains("super-secret-name"),
            "summary must not leak the raw API server message: {summary}"
        );
    }

    #[test]
    fn error_log_summary_non_kube_variant_passes_through_unchanged() {
        let err = Error::AppDefaults("missing entry for AppType::Radarr".to_string());
        assert_eq!(err.log_summary(), err.to_string());
    }

    #[test]
    fn error_public_summary_kube_variant_collapses_non_api_with_no_passthrough() {
        let err = Error::Kube(kube::Error::LinesCodecMaxLineLengthExceeded);
        let summary = err.public_summary();
        assert_ne!(summary, err.to_string());
    }

    #[test]
    fn error_public_summary_non_kube_variant_passes_through_unchanged() {
        let err = Error::AppDefaults("missing entry for AppType::Radarr".to_string());
        assert_eq!(err.public_summary(), err.to_string());
    }

    // ---- result_to_condition golden tests ----

    #[test]
    fn result_to_condition_api_error_message_is_byte_identical_to_pre_sanitization() {
        // Golden test (#443): an ApiResponse whose body would leak through the raw Display
        // must still produce exactly the pre-existing sanitized Condition message.
        let err = servarr_api::ApiError::ApiResponse {
            status: 401,
            body: "super-secret-leaky-body".to_string(),
        };
        let result: Result<(), TenantSafeMessage> = Err(TenantSafeMessage::from(err));
        let spec = ConditionSpec {
            condition_type: "Restore",
            ok_reason: "Succeeded",
            ok_message: "restore succeeded",
            fail_reason: "Failed",
            fail_log: "restore failed",
        };
        let condition = result_to_condition(result, spec, "my-app", "2026-07-31T00:00:00Z");
        assert_eq!(condition.message, "HTTP API error (status: 401)");
    }

    // ---- is_missing_data_torrent / build_download_health_condition (#483) ----

    fn torrent(id: i64, error: i64, error_string: &str) -> servarr_api::TorrentInfo {
        servarr_api::TorrentInfo {
            id,
            name: format!("torrent-{id}"),
            error,
            error_string: error_string.to_string(),
            status: 0,
            hash_string: format!("hash-{id}"),
        }
    }

    #[test]
    fn is_missing_data_torrent_matches_no_data_found_case_insensitively() {
        let t = torrent(1, 3, "No Data Found! Ensure your drives are connected.");
        assert!(is_missing_data_torrent(&t));
    }

    #[test]
    fn is_missing_data_torrent_requires_nonzero_error() {
        // errorString can be stale/leftover even after the error clears; error == 0 is
        // the authoritative "no current problem" signal.
        let t = torrent(1, 0, "no data found!");
        assert!(!is_missing_data_torrent(&t));
    }

    #[test]
    fn is_missing_data_torrent_ignores_unrelated_errors() {
        let t = torrent(1, 1, "tracker gave a warning");
        assert!(!is_missing_data_torrent(&t));
    }

    #[test]
    fn is_missing_data_torrent_ignores_tracker_error_even_with_matching_text() {
        // error == 2 (TR_STAT_TRACKER_ERROR) carries an errorString supplied by the
        // torrent's tracker, not the local filesystem. A hostile tracker returning
        // "no data found" in its failure reason must not trigger removal (#483).
        let t = torrent(1, 2, "no data found! (tracker failure reason)");
        assert!(!is_missing_data_torrent(&t));
    }

    #[test]
    fn build_download_health_condition_all_recovered_is_ok() {
        let outcome = RemediationOutcome {
            removed: Vec::new(),
            confirmed_orphaned: Vec::new(),
            still_pending: Vec::new(),
        };
        let cond = build_download_health_condition(2, &outcome, "2026-08-05T00:00:00Z");
        assert_eq!(cond.status, "True");
        assert_eq!(cond.reason, "DataRecovered");
    }

    #[test]
    fn build_download_health_condition_removed_is_fail() {
        let outcome = RemediationOutcome {
            removed: vec!["hash1".to_string()],
            confirmed_orphaned: Vec::new(),
            still_pending: Vec::new(),
        };
        let cond = build_download_health_condition(1, &outcome, "2026-08-05T00:00:00Z");
        assert_eq!(cond.status, "False");
        assert_eq!(cond.reason, "MissingDataDetected");
        assert!(cond.message.contains("1 removed"));
    }

    #[test]
    fn build_download_health_condition_pending_is_fail() {
        let outcome = RemediationOutcome {
            removed: Vec::new(),
            confirmed_orphaned: Vec::new(),
            still_pending: vec!["hash1".to_string(), "hash2".to_string()],
        };
        let cond = build_download_health_condition(2, &outcome, "2026-08-05T00:00:00Z");
        assert_eq!(cond.status, "False");
        assert!(cond.message.contains("2 pending verify"));
    }

    #[test]
    fn build_download_health_condition_confirmed_orphaned_is_fail_and_not_recovered() {
        // Verified as still erroring but not removed because auto_remove is disabled (#498) --
        // must not be counted as "recovered".
        let outcome = RemediationOutcome {
            removed: Vec::new(),
            confirmed_orphaned: vec!["hash1".to_string()],
            still_pending: Vec::new(),
        };
        let cond = build_download_health_condition(1, &outcome, "2026-08-05T00:00:00Z");
        assert_eq!(cond.status, "False");
        assert!(cond.message.contains("1 confirmed orphaned"));
        assert!(cond.message.contains("0 removed"));
    }

    // ---- poll_until_settled (#483) ----

    #[tokio::test]
    async fn poll_until_settled_returns_immediately_when_not_checking() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "torrents": [{"id": 1, "name": "x", "error": 3, "errorString": "no data found", "status": 0, "hashString": "hash-1"}]
                }
            })))
            .mount(&mock_server)
            .await;

        let client = servarr_api::TransmissionClient::new(&mock_server.uri(), None).unwrap();
        let settled = poll_until_settled(&client, &["hash-1"]).await;
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].status, 0);
    }

    #[tokio::test]
    async fn poll_until_settled_gives_up_and_returns_empty_when_still_checking() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "torrents": [{"id": 1, "name": "x", "error": 3, "errorString": "no data found", "status": 2, "hashString": "hash-1"}]
                }
            })))
            .mount(&mock_server)
            .await;

        let client = servarr_api::TransmissionClient::new(&mock_server.uri(), None).unwrap();
        let settled = poll_until_settled(&client, &["hash-1"]).await;
        assert!(
            settled.is_empty(),
            "still-checking torrents must not be treated as settled"
        );
    }

    // ---- remediate_stale_torrents (#483) ----

    #[tokio::test]
    async fn remediate_stale_torrents_removes_torrent_still_erroring_after_verify() {
        let mock_server = MockServer::start().await;

        // torrent-verify: always succeeds
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-verify"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success", "arguments": {}
            })))
            .mount(&mock_server)
            .await;
        // torrent-get: already settled (status 0), still erroring — used for both the
        // initial detect call and the post-verify poll.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(serde_json::json!({"method": "torrent-get"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "torrents": [{"id": 1, "name": "x", "error": 3, "errorString": "no data found", "status": 0, "hashString": "hash-1"}]
                }
            })))
            .mount(&mock_server)
            .await;
        // torrent-remove: expect exactly one call, addressed by hash (not the numeric id) and
        // with delete-local-data=false
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-remove", "arguments": {"ids": ["hash-1"], "delete-local-data": false}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success", "arguments": {}
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = servarr_api::TransmissionClient::new(&mock_server.uri(), None).unwrap();
        let stale = vec![torrent(1, 3, "no data found")];
        let outcome = remediate_stale_torrents(&client, &stale, true).await;

        assert_eq!(outcome.removed, vec!["hash-1".to_string()]);
        assert!(outcome.confirmed_orphaned.is_empty());
        assert!(outcome.still_pending.is_empty());
    }

    #[tokio::test]
    async fn remediate_stale_torrents_does_not_remove_when_auto_remove_disabled() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-verify"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success", "arguments": {}
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(serde_json::json!({"method": "torrent-get"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "torrents": [{"id": 1, "name": "x", "error": 3, "errorString": "no data found", "status": 0, "hashString": "hash-1"}]
                }
            })))
            .mount(&mock_server)
            .await;
        // torrent-remove must never be called when auto_remove is disabled, even for a
        // torrent confirmed still erroring after verify (#498).
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-remove"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success", "arguments": {}
            })))
            .expect(0)
            .mount(&mock_server)
            .await;

        let client = servarr_api::TransmissionClient::new(&mock_server.uri(), None).unwrap();
        let stale = vec![torrent(1, 3, "no data found")];
        let outcome = remediate_stale_torrents(&client, &stale, false).await;

        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.confirmed_orphaned, vec!["hash-1".to_string()]);
        assert!(outcome.still_pending.is_empty());
    }

    #[tokio::test]
    async fn remediate_stale_torrents_does_not_remove_while_still_checking() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-verify"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success", "arguments": {}
            })))
            .mount(&mock_server)
            .await;
        // Always reports "still verifying" (status 2) — poll never settles.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(serde_json::json!({"method": "torrent-get"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "torrents": [{"id": 1, "name": "x", "error": 3, "errorString": "no data found", "status": 2, "hashString": "hash-1"}]
                }
            })))
            .mount(&mock_server)
            .await;
        // torrent-remove must never be called while still checking.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-remove"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success", "arguments": {}
            })))
            .expect(0)
            .mount(&mock_server)
            .await;

        let client = servarr_api::TransmissionClient::new(&mock_server.uri(), None).unwrap();
        let stale = vec![torrent(1, 3, "no data found")];
        let outcome = remediate_stale_torrents(&client, &stale, true).await;

        assert!(outcome.removed.is_empty());
        assert!(outcome.confirmed_orphaned.is_empty());
        assert_eq!(outcome.still_pending, vec!["hash-1".to_string()]);
    }

    #[tokio::test]
    async fn remediate_stale_torrents_skips_verify_for_torrent_already_checking() {
        let mock_server = MockServer::start().await;

        // torrent-verify must never be called for a torrent that's already checking —
        // restarting the hash on every reconcile would mean it never converges.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-verify"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success", "arguments": {}
            })))
            .expect(0)
            .mount(&mock_server)
            .await;

        let client = servarr_api::TransmissionClient::new(&mock_server.uri(), None).unwrap();
        let mut t = torrent(1, 3, "no data found");
        t.status = 2; // already verifying
        let outcome = remediate_stale_torrents(&client, &[t], true).await;

        assert!(outcome.removed.is_empty());
        assert!(outcome.confirmed_orphaned.is_empty());
        assert_eq!(outcome.still_pending, vec!["hash-1".to_string()]);
    }

    // ---- report_stale_torrents (#483) ----

    #[tokio::test]
    async fn report_stale_torrents_publishes_download_data_missing_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        let app = make_test_app("my-transmission", "test", AppType::Transmission);
        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let stale = vec![torrent(1, 3, "sensitive-torrent-name")];
        let outcome = RemediationOutcome {
            removed: vec!["hash-1".to_string()],
            confirmed_orphaned: Vec::new(),
            still_pending: Vec::new(),
        };
        report_stale_torrents(&recorder, &obj_ref, &stale, &outcome).await;

        let bodies = event_post_bodies(&mock_server).await;
        assert_eq!(bodies.len(), 1, "expected exactly one Event POST");
        assert_eq!(bodies[0]["reason"], "DownloadDataMissing");
        assert_eq!(bodies[0]["type"], "Warning");
        let note = bodies[0]["note"].as_str().unwrap();
        assert!(note.contains("1 removed as orphaned"));
        assert!(
            note.contains('1'),
            "note should carry the torrent id: {note}"
        );
        assert!(
            !note.contains("sensitive-torrent-name"),
            "torrent names (tracker-controlled content) must not reach the tenant-visible \
             Event note: {note}"
        );
    }

    #[tokio::test]
    async fn report_stale_torrents_publishes_nothing_when_fully_recovered() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        let app = make_test_app("my-transmission", "test", AppType::Transmission);
        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let stale = vec![torrent(1, 3, "no data found")];
        let outcome = RemediationOutcome {
            removed: Vec::new(),
            confirmed_orphaned: Vec::new(),
            still_pending: Vec::new(),
        };
        report_stale_torrents(&recorder, &obj_ref, &stale, &outcome).await;

        let bodies = event_post_bodies(&mock_server).await;
        assert!(
            bodies.is_empty(),
            "a fully-recovered outcome should not publish a Warning Event"
        );
    }

    // ---- check_download_client_health credential fail-closed (#483) ----

    #[tokio::test]
    async fn check_download_client_health_fails_closed_on_partial_credentials() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        // Secret has "username" but not "password" — a partial read.
        mount_secret_mock(
            &mock_server,
            "test",
            "tx-admin-creds",
            json!({"username": "YWRtaW4="}),
        )
        .await;

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: None,
            auto_remove_orphaned_torrents: false,
        });
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });
        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let access = resolve_transmission_access(&client, &app, "test", &mock_server.uri()).await;
        assert!(
            access.as_ref().is_ok_and(|a| a.credentials_incomplete),
            "expected a resolved access with credentials_incomplete=true, got {access:?}"
        );

        let cond = check_download_client_health(&app, &recorder, &obj_ref, Some(&access)).await;

        let cond = cond.expect("a partial credential read must still surface a condition");
        assert_eq!(cond.condition_type, condition_types::DOWNLOAD_DATA_HEALTHY);
        assert_eq!(cond.status, "Unknown");
        assert_eq!(cond.reason, "CredentialReadError");

        // No RPC call to Transmission should ever have been attempted.
        let events = event_post_bodies(&mock_server).await;
        assert!(
            events.is_empty(),
            "no Event should be published on a credential-read failure"
        );
    }

    #[tokio::test]
    async fn check_download_client_health_fails_closed_on_total_credential_read_failure() {
        // adminCredentials is configured, but the secret doesn't exist at all (deleted,
        // renamed, RBAC revoked) -- both username and password reads fail, landing back at
        // (None, None). Must still fail closed, not be mistaken for "adminCredentials unset".
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        // Deliberately no mount_secret_mock call -- the GET 404s.

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: None,
            auto_remove_orphaned_torrents: false,
        });
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });
        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let access = resolve_transmission_access(&client, &app, "test", &mock_server.uri()).await;
        assert!(
            access.as_ref().is_ok_and(|a| a.credentials_incomplete),
            "expected credentials_incomplete=true on total read failure, got {access:?}"
        );

        let cond = check_download_client_health(&app, &recorder, &obj_ref, Some(&access)).await;

        let cond = cond.expect("a total credential read failure must still surface a condition");
        assert_eq!(cond.status, "Unknown");
        assert_eq!(cond.reason, "CredentialReadError");

        let events = event_post_bodies(&mock_server).await;
        assert!(
            events.is_empty(),
            "no Event should be published on a credential-read failure"
        );
    }

    // ---- transmission_access is shared across both health checks (#499) ----

    #[tokio::test]
    async fn transmission_access_is_shared_between_health_checks() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        // adminCredentials secret — read once by resolve_transmission_access, reused by both
        // checks (2 Secret GETs total, not 4).
        mount_secret_mock(
            &mock_server,
            "test",
            "tx-admin-creds",
            json!({"username": "YWRtaW4=", "password": "c2VjcmV0"}),
        )
        .await;
        // apiKeySecret — only needed to pass check_api_health's generic per-app-type gate;
        // its content is unused by the Transmission arm.
        mount_secret_mock(
            &mock_server,
            "test",
            "tx-api-key",
            json!({"api-key": "dW51c2Vk"}),
        )
        .await;

        // Only the first RPC call on the shared client should trigger the session-ID
        // handshake -- if the client were rebuilt for the second check, it would handshake
        // again, and this `.expect(1)` would fail when the mock server is checked on drop.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409)
                    .append_header("X-Transmission-Session-Id", "sess-shared"),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "session-get"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {"version": "4.0.0"}
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-get"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {"torrents": []}
            })))
            .mount(&mock_server)
            .await;

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: None,
            auto_remove_orphaned_torrents: false,
        });
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });
        app.spec.api_key_secret = Some("tx-api-key".into());
        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let access = resolve_transmission_access(&client, &app, "test", &mock_server.uri()).await;
        assert!(access.is_ok(), "expected access to resolve, got {access:?}");

        let (health_cond, _update_cond) = check_api_health(&client, &app, Some(&access)).await;
        assert_eq!(
            health_cond.map(|c| c.status),
            Some("True".to_string()),
            "expected the shared client's session-get to succeed"
        );

        let download_cond =
            check_download_client_health(&app, &recorder, &obj_ref, Some(&access)).await;
        assert_eq!(
            download_cond.map(|c| c.reason),
            Some("NoStaleData".to_string()),
            "expected the shared client's torrent-get to succeed with no stale torrents"
        );
    }

    // ---- check_api_health: apiKeySecret is only required by the arms that use it (#509) ----

    #[tokio::test]
    async fn transmission_health_check_without_api_key_secret_reports_healthy() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        // adminCredentials secret — lets resolve_transmission_access build an authenticated
        // client. Deliberately no apiKeySecret: the Transmission arm must depend only on
        // apiHealthCheck.enabled (#509).
        mount_secret_mock(
            &mock_server,
            "test",
            "tx-admin-creds",
            json!({"username": "YWRtaW4=", "password": "c2VjcmV0"}),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "session-get"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {"version": "4.0.0"}
            })))
            .mount(&mock_server)
            .await;

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: None,
            auto_remove_orphaned_torrents: false,
        });
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });
        // No api_key_secret set.

        let access = resolve_transmission_access(&client, &app, "test", &mock_server.uri()).await;
        assert!(access.is_ok(), "expected access to resolve, got {access:?}");

        let (health_cond, _update_cond) = check_api_health(&client, &app, Some(&access)).await;
        assert_eq!(
            health_cond.map(|c| c.status),
            Some("True".to_string()),
            "Transmission must get an APP_HEALTHY condition without apiKeySecret (#509)"
        );
    }

    #[tokio::test]
    async fn servarr_health_check_without_api_key_secret_reports_nothing() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let mut app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: None,
            auto_remove_orphaned_torrents: false,
        });
        // No api_key_secret set.

        let (health_cond, _update_cond) = check_api_health(&client, &app, None).await;
        assert_eq!(
            health_cond.map(|c| c.status),
            None,
            "Sonarr still requires apiKeySecret for a health condition (#509 preserves this)"
        );
    }

    // ---- transmission_verify_client reuses the shared client (#508) ----

    #[tokio::test]
    async fn transmission_verify_client_reuses_shared_client() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        mount_secret_mock(
            &mock_server,
            "test",
            "tx-admin-creds",
            json!({"username": "YWRtaW4=", "password": "c2VjcmV0"}),
        )
        .await;
        // The session-ID handshake fires exactly once, when the shared client is primed. If
        // the verify client were built fresh, it would handshake a second time and this
        // `.expect(1)` would fail when the mock server is checked on drop (#508).
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409)
                    .append_header("X-Transmission-Session-Id", "sess-shared"),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock_server)
            .await;
        // session-get only succeeds when it echoes the shared client's cached session ID — a
        // fresh client would send no header and fail to match, so this proves the verify path
        // returned the shared client (#508).
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "session-get"}),
            ))
            .and(header("X-Transmission-Session-Id", "sess-shared"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {"version": "4.0.0"}
            })))
            .mount(&mock_server)
            .await;

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });

        let access = resolve_transmission_access(&client, &app, "test", &mock_server.uri()).await;
        assert!(access.is_ok(), "expected access to resolve, got {access:?}");

        // Prime the shared client's session-ID cache (handshake #1).
        access.as_ref().unwrap().client.session_get().await.unwrap();

        // The verify client must be the shared client: its session_get reuses the cached
        // header and succeeds without a second handshake.
        let verify =
            transmission_verify_client(Some(&access), &mock_server.uri(), "admin", "secret")
                .unwrap();
        verify.session_get().await.unwrap();
    }

    // ---- apiHealthCheck.intervalSeconds throttle (#506) ----

    #[test]
    fn is_health_poll_throttled_within_interval() {
        let now = chrono_now();
        let recent = (chrono::Utc::now() - chrono::Duration::seconds(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let existing = Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "True".to_string(),
            reason: "Healthy".to_string(),
            message: "API responded healthy".to_string(),
            last_transition_time: recent,
        };
        assert!(is_health_poll_throttled(Some(&existing), Some(60), &now));
    }

    #[test]
    fn is_health_poll_throttled_outside_interval() {
        let now = chrono_now();
        let stale = (chrono::Utc::now() - chrono::Duration::seconds(120))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let existing = Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "True".to_string(),
            reason: "Healthy".to_string(),
            message: "API responded healthy".to_string(),
            last_transition_time: stale,
        };
        assert!(!is_health_poll_throttled(Some(&existing), Some(60), &now));
    }

    #[test]
    fn is_health_poll_throttled_defaults_to_60_seconds_when_omitted() {
        let now = chrono_now();
        let within_default = (chrono::Utc::now() - chrono::Duration::seconds(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let existing = Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "True".to_string(),
            reason: "Healthy".to_string(),
            message: "API responded healthy".to_string(),
            last_transition_time: within_default,
        };
        // interval omitted → default 60s window, so a 30s-old condition is still throttled.
        assert!(is_health_poll_throttled(Some(&existing), None, &now));
    }

    #[test]
    fn is_health_poll_throttled_never_throttles_without_existing_or_bad_timestamp() {
        let now = chrono_now();
        assert!(!is_health_poll_throttled(None, Some(60), &now));

        let bad = Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "True".to_string(),
            reason: "Healthy".to_string(),
            message: "API responded healthy".to_string(),
            last_transition_time: "not-a-timestamp".to_string(),
        };
        assert!(!is_health_poll_throttled(Some(&bad), Some(60), &now));

        // intervalSeconds: 0 means "poll on every reconcile" — never throttled.
        let zero = Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "True".to_string(),
            reason: "Healthy".to_string(),
            message: "API responded healthy".to_string(),
            last_transition_time: now.clone(),
        };
        assert!(!is_health_poll_throttled(Some(&zero), Some(0), &now));
    }

    #[test]
    fn is_health_poll_throttled_does_not_freeze_non_true_conditions() {
        let now = chrono_now();
        let recent = (chrono::Utc::now() - chrono::Duration::seconds(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        // A "False" condition (MissingDataDetected, credential-read error) must never be
        // frozen: re-poll on the next reconcile so recovery is detected promptly instead
        // of waiting out the interval.
        let false_cond = Condition {
            condition_type: condition_types::DOWNLOAD_DATA_HEALTHY.to_string(),
            status: "False".to_string(),
            reason: "MissingDataDetected".to_string(),
            message: "stale torrents still missing".to_string(),
            last_transition_time: recent.clone(),
        };
        assert!(!is_health_poll_throttled(
            Some(&false_cond),
            Some(3600),
            &now
        ));

        // Same for "Unknown" — never a positive assertion, never rate-limited.
        let unknown_cond = Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "Unknown".to_string(),
            reason: "ClientBuildError".to_string(),
            message: "could not reach the API".to_string(),
            last_transition_time: recent,
        };
        assert!(!is_health_poll_throttled(
            Some(&unknown_cond),
            Some(3600),
            &now
        ));
    }

    #[test]
    fn is_health_poll_throttled_fails_open_on_future_timestamp() {
        let now = chrono_now();
        let future = (chrono::Utc::now() + chrono::Duration::hours(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let existing = Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "True".to_string(),
            reason: "Healthy".to_string(),
            message: "API responded healthy".to_string(),
            last_transition_time: future,
        };
        // A future-dated lastTransitionTime (clock skew, hand-edit) must not freeze the
        // poll — not even at intervalSeconds: 0, where `current < last` would otherwise
        // throttle on every reconcile until the future passes.
        assert!(!is_health_poll_throttled(Some(&existing), Some(0), &now));
        assert!(!is_health_poll_throttled(Some(&existing), Some(3600), &now));
    }

    // Property tests for the throttle's documented invariants (see is_health_poll_throttled):
    // if a poll is throttled, the condition is a positive "True" assertion whose
    // lastTransitionTime is parseable, not in the future, and inside the interval window;
    // the function never panics on arbitrary (CRD/attacker-shaped) input.
    mod throttle_proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn only_throttles_healthy_within_window(
                status in proptest::sample::select(&["True", "False", "Unknown"]),
                offset_secs in -7_200i64..=7_200i64,
                interval in 0u32..=3_600u32,
            ) {
                let now = chrono::Utc::now();
                let last = (now + chrono::Duration::seconds(offset_secs))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let now_str = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let existing = Condition {
                    condition_type: condition_types::APP_HEALTHY.to_string(),
                    status: status.to_string(),
                    reason: "Healthy".to_string(),
                    message: "proptest".to_string(),
                    last_transition_time: last,
                };
                let throttled = is_health_poll_throttled(Some(&existing), Some(interval), &now_str);
                if throttled {
                    prop_assert_eq!(status, "True", "only a positive health assertion is rate-limited");
                    prop_assert!(interval > 0, "interval 0 must never throttle");
                    prop_assert!(offset_secs <= 0, "future-dated timestamp must not throttle");
                    prop_assert!(
                        -offset_secs < i64::from(interval),
                        "throttled condition must fall inside the interval window"
                    );
                }
            }

            #[test]
            fn never_panics_on_arbitrary_input(
                last in "\\PC*",
                now in "\\PC*",
                status in "\\PC*",
                interval in 0u32..=u32::MAX,
            ) {
                let existing = Condition {
                    condition_type: "AppHealthy".to_string(),
                    status,
                    reason: "r".to_string(),
                    message: "m".to_string(),
                    last_transition_time: last,
                };
                // Garbage status strings and timestamps must fail open, not panic.
                let _ = is_health_poll_throttled(Some(&existing), Some(interval), &now);
                let _ = is_health_poll_throttled(None, Some(interval), &now);
            }
        }
    }

    #[tokio::test]
    async fn check_api_health_throttles_within_interval_seconds() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        mount_secret_mock(
            &mock_server,
            "test",
            "tx-admin-creds",
            json!({"username": "YWRtaW4=", "password": "c2VjcmV0"}),
        )
        .await;
        // If the throttle is broken, the health probe would hit session-get — the
        // `.expect(0)` fails the mock server verify and catches the regression.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "session-get"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {"version": "4.0.0"}
            })))
            .expect(0)
            .mount(&mock_server)
            .await;

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: Some(3600),
            auto_remove_orphaned_torrents: false,
        });
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });

        // Existing APP_HEALTHY condition from a poll moments ago — still inside the 3600s
        // window, so the probe must be skipped and the condition reused unchanged.
        let recent = chrono_now();
        app.status = Some(ServarrAppStatus {
            ready: true,
            ready_replicas: 1,
            observed_generation: 1,
            conditions: vec![Condition {
                condition_type: condition_types::APP_HEALTHY.to_string(),
                status: "True".to_string(),
                reason: "Healthy".to_string(),
                message: "API responded healthy".to_string(),
                last_transition_time: recent.clone(),
            }],
            backup_status: None,
        });

        let access = resolve_transmission_access(&client, &app, "test", &mock_server.uri()).await;
        assert!(access.is_ok(), "expected access to resolve, got {access:?}");

        let (health_cond, update_cond) = check_api_health(&client, &app, Some(&access)).await;
        assert_eq!(
            health_cond.map(|c| c.last_transition_time),
            Some(recent),
            "throttled poll must reuse the existing APP_HEALTHY condition unchanged"
        );
        assert!(
            update_cond.is_none(),
            "no existing update condition to preserve, so none is reported"
        );
        mock_server.verify().await;
    }

    #[tokio::test]
    async fn check_api_health_polls_when_interval_elapsed() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        mount_secret_mock(
            &mock_server,
            "test",
            "tx-admin-creds",
            json!({"username": "YWRtaW4=", "password": "c2VjcmV0"}),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "session-get"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {"version": "4.0.0"}
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: Some(3600),
            auto_remove_orphaned_torrents: false,
        });
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });

        // Existing condition from 2 hours ago — outside the 3600s window, so the poll runs.
        let stale = (chrono::Utc::now() - chrono::Duration::hours(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        app.status = Some(ServarrAppStatus {
            ready: true,
            ready_replicas: 1,
            observed_generation: 1,
            conditions: vec![Condition {
                condition_type: condition_types::APP_HEALTHY.to_string(),
                status: "True".to_string(),
                reason: "Healthy".to_string(),
                message: "API responded healthy".to_string(),
                last_transition_time: stale.clone(),
            }],
            backup_status: None,
        });

        let access = resolve_transmission_access(&client, &app, "test", &mock_server.uri()).await;
        assert!(access.is_ok(), "expected access to resolve, got {access:?}");

        let (health_cond, _update_cond) = check_api_health(&client, &app, Some(&access)).await;
        let health = health_cond.expect("elapsed interval must re-run the health poll");
        assert_eq!(health.status, "True");
        assert_ne!(
            health.last_transition_time, stale,
            "poll must produce a fresh condition, not reuse the stale one"
        );
        mock_server.verify().await;
    }

    #[tokio::test]
    async fn check_download_client_health_throttles_within_interval_seconds() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        mount_secret_mock(
            &mock_server,
            "test",
            "tx-admin-creds",
            json!({"username": "YWRtaW4=", "password": "c2VjcmV0"}),
        )
        .await;
        // If the throttle is broken, the destructive self-heal would run torrent-get — the
        // `.expect(0)` fails the mock server verify and catches the regression.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-get"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {"torrents": []}
            })))
            .expect(0)
            .mount(&mock_server)
            .await;

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: Some(3600),
            auto_remove_orphaned_torrents: true, // the destructive opt-in must be throttled too
        });
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });

        let recent = chrono_now();
        app.status = Some(ServarrAppStatus {
            ready: true,
            ready_replicas: 1,
            observed_generation: 1,
            conditions: vec![Condition {
                condition_type: condition_types::DOWNLOAD_DATA_HEALTHY.to_string(),
                status: "True".to_string(),
                reason: "NoStaleData".to_string(),
                message: "No torrents reporting missing data".to_string(),
                last_transition_time: recent.clone(),
            }],
            backup_status: None,
        });

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        // transmission_access is deliberately None here — the throttle must skip the pass
        // before it is ever consulted.
        let cond = check_download_client_health(&app, &recorder, &obj_ref, None).await;
        assert_eq!(
            cond.map(|c| c.last_transition_time),
            Some(recent),
            "throttled self-heal must reuse the existing DOWNLOAD_DATA_HEALTHY condition unchanged"
        );
        mock_server.verify().await;
    }

    #[tokio::test]
    async fn check_download_client_health_polls_when_interval_elapsed() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        mount_secret_mock(
            &mock_server,
            "test",
            "tx-admin-creds",
            json!({"username": "YWRtaW4=", "password": "c2VjcmV0"}),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(body_partial_json(
                serde_json::json!({"method": "torrent-get"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {"torrents": []}
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut app = make_test_app("my-transmission", "test", AppType::Transmission);
        app.spec.api_health_check = Some(servarr_crds::ApiHealthCheckSpec {
            enabled: true,
            interval_seconds: Some(3600),
            auto_remove_orphaned_torrents: true,
        });
        app.spec.admin_credentials = Some(servarr_crds::AdminCredentialsSpec {
            secret_name: "tx-admin-creds".into(),
        });

        // Existing condition from 2 hours ago — outside the 3600s window, so the pass runs.
        let stale = (chrono::Utc::now() - chrono::Duration::hours(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        app.status = Some(ServarrAppStatus {
            ready: true,
            ready_replicas: 1,
            observed_generation: 1,
            conditions: vec![Condition {
                condition_type: condition_types::DOWNLOAD_DATA_HEALTHY.to_string(),
                status: "True".to_string(),
                reason: "NoStaleData".to_string(),
                message: "No torrents reporting missing data".to_string(),
                last_transition_time: stale.clone(),
            }],
            backup_status: None,
        });

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let access = resolve_transmission_access(&client, &app, "test", &mock_server.uri()).await;
        assert!(access.is_ok(), "expected access to resolve, got {access:?}");

        let cond = check_download_client_health(&app, &recorder, &obj_ref, Some(&access)).await;
        let cond = cond.expect("elapsed interval must re-run the self-heal pass");
        assert_eq!(cond.status, "True");
        assert_eq!(cond.reason, "NoStaleData");
        assert_ne!(
            cond.last_transition_time, stale,
            "self-heal must produce a fresh condition, not reuse the stale one"
        );
        mock_server.verify().await;
    }

    // ---- json_is_subset ----

    #[test]
    fn json_is_subset_both_empty_objects() {
        assert!(json_is_subset(&json!({}), &json!({})));
    }

    #[test]
    fn json_is_subset_extra_keys_in_actual() {
        assert!(json_is_subset(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
    }

    #[test]
    fn json_is_subset_value_mismatch() {
        assert!(!json_is_subset(&json!({"a": 1}), &json!({"a": 2})));
    }

    #[test]
    fn json_is_subset_missing_key_in_actual() {
        assert!(!json_is_subset(&json!({"a": 1}), &json!({})));
    }

    #[test]
    fn json_is_subset_missing_false_bool_not_drift() {
        // Kubernetes omits readOnly:false from actual; desired=false must not trigger drift.
        assert!(json_is_subset(&json!({"readOnly": false}), &json!({})));
    }

    #[test]
    fn json_is_subset_missing_true_bool_is_drift() {
        assert!(!json_is_subset(&json!({"readOnly": true}), &json!({})));
    }

    #[test]
    fn json_is_subset_missing_zero_int_not_drift() {
        assert!(json_is_subset(&json!({"port": 0}), &json!({})));
    }

    #[test]
    fn json_is_subset_missing_nonzero_int_is_drift() {
        assert!(!json_is_subset(&json!({"port": 8080}), &json!({})));
    }

    #[test]
    fn json_is_subset_nested_objects_extra_keys() {
        assert!(json_is_subset(
            &json!({"a": {"b": 1}}),
            &json!({"a": {"b": 1, "c": 2}})
        ));
    }

    #[test]
    fn json_is_subset_arrays_same() {
        assert!(json_is_subset(&json!([1, 2, 3]), &json!([1, 2, 3])));
    }

    #[test]
    fn json_is_subset_arrays_different_lengths() {
        assert!(!json_is_subset(&json!([1, 2]), &json!([1, 2, 3])));
    }

    #[test]
    fn json_is_subset_arrays_different_values() {
        assert!(!json_is_subset(&json!([1, 2, 3]), &json!([1, 2, 4])));
    }

    #[test]
    fn json_is_subset_null_vs_null() {
        assert!(json_is_subset(&json!(null), &json!(null)));
    }

    #[test]
    fn json_is_subset_string_equality() {
        assert!(json_is_subset(&json!("hello"), &json!("hello")));
    }

    #[test]
    fn json_is_subset_string_inequality() {
        assert!(!json_is_subset(&json!("hello"), &json!("world")));
    }

    #[test]
    fn json_is_subset_number_equality() {
        assert!(json_is_subset(&json!(42), &json!(42)));
    }

    #[test]
    fn json_is_subset_mixed_types() {
        assert!(!json_is_subset(&json!(1), &json!("1")));
    }

    #[test]
    fn json_is_subset_deeply_nested_match() {
        let desired = json!({"a": {"b": {"c": {"d": 1}}}});
        let actual = json!({"a": {"b": {"c": {"d": 1, "e": 2}, "f": 3}}, "g": 4});
        assert!(json_is_subset(&desired, &actual));
    }

    #[test]
    fn json_is_subset_deeply_nested_mismatch() {
        let desired = json!({"a": {"b": {"c": {"d": 1}}}});
        let actual = json!({"a": {"b": {"c": {"d": 99}}}});
        assert!(!json_is_subset(&desired, &actual));
    }

    // ---- json_diff_paths ----

    #[test]
    fn json_diff_paths_both_empty_objects() {
        let result = json_diff_paths(&json!({}), &json!({}), String::new());
        assert!(result.is_empty());
    }

    #[test]
    fn json_diff_paths_missing_key() {
        let result = json_diff_paths(&json!({"key": 1}), &json!({}), String::new());
        assert_eq!(result, vec!["key: missing in actual"]);
    }

    #[test]
    fn json_diff_paths_different_value() {
        let result = json_diff_paths(&json!({"key": 1}), &json!({"key": 2}), String::new());
        assert_eq!(result, vec!["key: 1 vs 2"]);
    }

    #[test]
    fn json_diff_paths_nested_difference() {
        let result = json_diff_paths(
            &json!({"parent": {"child": 1}}),
            &json!({"parent": {"child": 2}}),
            String::new(),
        );
        assert_eq!(result, vec!["parent.child: 1 vs 2"]);
    }

    #[test]
    fn json_diff_paths_array_length_mismatch() {
        let result = json_diff_paths(&json!({"a": [1, 2]}), &json!({"a": [1]}), String::new());
        assert_eq!(result, vec!["a: array length 2 vs 1"]);
    }

    #[test]
    fn json_diff_paths_array_element_difference() {
        let result = json_diff_paths(&json!({"a": [1, 2]}), &json!({"a": [1, 3]}), String::new());
        assert_eq!(result, vec!["a[1]: 2 vs 3"]);
    }

    #[test]
    fn json_diff_paths_multiple_differences() {
        let result = json_diff_paths(
            &json!({"a": 1, "b": 2}),
            &json!({"a": 10, "b": 20}),
            String::new(),
        );
        assert_eq!(result.len(), 2);
        assert!(result.contains(&"a: 1 vs 10".to_string()));
        assert!(result.contains(&"b: 2 vs 20".to_string()));
    }

    #[test]
    fn json_diff_paths_root_path_empty_no_leading_dot() {
        let result = json_diff_paths(&json!({"x": 1}), &json!({"x": 2}), String::new());
        // Should be "x: ..." not ".x: ..."
        assert!(result[0].starts_with("x:"));
    }

    // ---- app_type_to_kind ----

    #[test]
    fn app_type_to_kind_sonarr() {
        assert!(matches!(
            app_type_to_kind(&AppType::Sonarr),
            Some(AppKind::Sonarr)
        ));
    }

    #[test]
    fn app_type_to_kind_radarr() {
        assert!(matches!(
            app_type_to_kind(&AppType::Radarr),
            Some(AppKind::Radarr)
        ));
    }

    #[test]
    fn app_type_to_kind_lidarr() {
        assert!(matches!(
            app_type_to_kind(&AppType::Lidarr),
            Some(AppKind::Lidarr)
        ));
    }

    #[test]
    fn app_type_to_kind_prowlarr() {
        assert!(matches!(
            app_type_to_kind(&AppType::Prowlarr),
            Some(AppKind::Prowlarr)
        ));
    }

    #[test]
    fn app_type_to_kind_unsupported_returns_none() {
        assert!(app_type_to_kind(&AppType::Sabnzbd).is_none());
    }

    // ---- chrono_now ----

    #[test]
    fn chrono_now_returns_valid_iso8601() {
        let now = chrono_now();
        assert!(now.contains('T'), "should contain T separator: {now}");
        assert!(now.ends_with('Z'), "should end with Z: {now}");
    }

    // ---- normalize_backup_schedule ----

    #[test]
    fn normalize_backup_schedule_pads_standard_five_field_cron() {
        // 5-field standard cron gets a "0" seconds field, then parses under the
        // `cron` crate (which rejects bare 5-field input).
        let normalized = normalize_backup_schedule("0 3 * * *");
        assert_eq!(normalized, "0 0 3 * * *");
        assert!(cron::Schedule::from_str(&normalized).is_ok());
        // Surrounding whitespace is trimmed before padding.
        assert_eq!(normalize_backup_schedule("  0 3 * * *  "), "0 0 3 * * *");
        // 6- and 7-field expressions pass through unchanged.
        assert_eq!(normalize_backup_schedule("0 0 3 * * *"), "0 0 3 * * *");
        assert_eq!(
            normalize_backup_schedule("0 0 3 * * * 2099"),
            "0 0 3 * * * 2099"
        );
        // Fewer than 5 fields is left as-is and rejected downstream by the parser.
        assert_eq!(normalize_backup_schedule("0 3 * *"), "0 3 * *");
        assert!(cron::Schedule::from_str(&normalize_backup_schedule("0 3 * *")).is_err());
        // Whitespace-only normalizes to empty (caller's guard treats it as unset).
        assert_eq!(normalize_backup_schedule("   "), "");
    }

    // ---- maybe_run_backup ----

    /// For the `Api` variant specifically, `log_summary()` and `public_summary()` collapse to the
    /// same status-code string — this guards that equivalence, not the log-only-vs-tenant-safe
    /// distinction (see `maybe_run_backup_secret_read_error_sanitizes_backup_status` below for
    /// that, which uses a non-`Api` `kube::Error` to force the two methods to actually diverge).
    #[tokio::test]
    async fn maybe_run_backup_secret_read_error_sanitizes_status_message() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let mut app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        app.spec.api_key_secret = Some("sonarr-api-key".into());
        app.spec.backup = Some(servarr_crds::BackupSpec {
            enabled: true,
            schedule: "0 3 * * *".into(),
            retention_count: 5,
        });

        // Secret read fails with a 403 whose message/reason echoes the secret name — must not
        // leak into the tenant-visible BackupStatus.
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/sonarr-api-key"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {},
                "status": "Failure",
                "message": "secrets \"sonarr-api-key\" is forbidden: User cannot get",
                "reason": "Forbidden",
                "code": 403
            })))
            .mount(&mock_server)
            .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let status = maybe_run_backup(&client, &app, &recorder, &obj_ref).await;

        let result = status
            .and_then(|s| s.last_backup_result)
            .expect("expected a last_backup_result for the secret-read failure");
        assert!(
            result.contains("403"),
            "should keep the status code: {result}"
        );
        assert!(
            !result.contains("sonarr-api-key"),
            "must not leak the raw API server message: {result}"
        );
    }

    // ---- print_crd ----

    #[test]
    fn print_crd_returns_ok() {
        assert!(print_crd().is_ok());
    }

    // ---- prowlarr_sync_exists ----

    #[tokio::test]
    async fn prowlarr_sync_exists_returns_true_when_prowlarr_with_sync() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        // Return a Prowlarr app with prowlarr_sync enabled
        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrAppList",
                "metadata": {},
                "items": [{
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "metadata": {
                        "name": "prowlarr",
                        "namespace": "test",
                        "uid": "prowl-uid",
                        "resourceVersion": "1"
                    },
                    "spec": {
                        "app": "Prowlarr",
                        "prowlarrSync": {
                            "enabled": true
                        }
                    }
                }]
            })))
            .mount(&mock_server)
            .await;

        let result = prowlarr_sync_exists(&client, "test").await;
        assert!(
            result,
            "should return true when Prowlarr with sync.enabled exists"
        );
    }

    #[tokio::test]
    async fn prowlarr_sync_exists_returns_false_when_no_prowlarr() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        // Return only a Sonarr app (no Prowlarr)
        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrAppList",
                "metadata": {},
                "items": [{
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "metadata": {
                        "name": "sonarr",
                        "namespace": "test",
                        "uid": "sonarr-uid",
                        "resourceVersion": "1"
                    },
                    "spec": { "app": "Sonarr" }
                }]
            })))
            .mount(&mock_server)
            .await;

        let result = prowlarr_sync_exists(&client, "test").await;
        assert!(!result, "should return false when no Prowlarr exists");
    }

    #[tokio::test]
    async fn prowlarr_sync_exists_returns_false_when_sync_disabled() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        // Prowlarr exists but sync is disabled
        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrAppList",
                "metadata": {},
                "items": [{
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "metadata": {
                        "name": "prowlarr",
                        "namespace": "test",
                        "uid": "prowl-uid",
                        "resourceVersion": "1"
                    },
                    "spec": {
                        "app": "Prowlarr",
                        "prowlarrSync": {
                            "enabled": false
                        }
                    }
                }]
            })))
            .mount(&mock_server)
            .await;

        let result = prowlarr_sync_exists(&client, "test").await;
        assert!(
            !result,
            "should return false when Prowlarr sync is disabled"
        );
    }

    #[tokio::test]
    async fn prowlarr_sync_exists_returns_false_on_api_error() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        // API returns 500
        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {},
                "status": "Failure",
                "message": "internal error",
                "reason": "InternalError",
                "code": 500
            })))
            .mount(&mock_server)
            .await;

        let result = prowlarr_sync_exists(&client, "test").await;
        assert!(!result, "should return false on API error");
    }

    // ---- Error display format ----

    #[test]
    fn error_display_kube_variant() {
        // Use FromUtf8 variant as a simple kube::Error to construct
        let invalid_bytes = vec![0xff, 0xfe];
        let utf8_err = String::from_utf8(invalid_bytes).unwrap_err();
        let kube_err = kube::Error::FromUtf8(utf8_err);
        let err = Error::Kube(kube_err);
        let display = format!("{err}");
        assert!(
            display.contains("Kubernetes API error"),
            "Kube error display should contain 'Kubernetes API error', got: {display}"
        );
    }

    #[test]
    fn error_display_serialization_variant() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err = Error::Serialization(json_err);
        let display = format!("{err}");
        assert!(
            display.contains("Serialization error"),
            "Serialization error display should contain 'Serialization error', got: {display}"
        );
    }

    #[test]
    fn error_debug_format_includes_variant_name() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err = Error::Serialization(json_err);
        let debug = format!("{err:?}");
        assert!(
            debug.contains("Serialization"),
            "Debug format should include variant name, got: {debug}"
        );
    }

    use crate::testutils::build_mock_client;

    async fn mount_secret_mock(
        mock_server: &MockServer,
        ns: &str,
        name: &str,
        data: serde_json::Value,
    ) {
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/namespaces/{ns}/secrets/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": { "name": name, "namespace": ns },
                "data": data
            })))
            .mount(mock_server)
            .await;
    }

    // ---- Helper: build a minimal ServarrApp for testing ----

    fn make_test_app(name: &str, ns: &str, app_type: AppType) -> ServarrApp {
        use servarr_crds::ServarrAppSpec;
        let spec = ServarrAppSpec {
            app: app_type,
            ..Default::default()
        };
        let mut app = ServarrApp::new(name, spec);
        app.metadata.namespace = Some(ns.into());
        app.metadata.uid = Some("test-uid-12345".into());
        app.metadata.resource_version = Some("1".into());
        app.metadata.generation = Some(1);
        app
    }

    // ---- update_status tests ----

    #[tokio::test]
    async fn update_status_ready_deployment() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);

        // GET deployment returns readyReplicas=1
        Mock::given(method("GET"))
            .and(path("/apis/apps/v1/namespaces/test/deployments/my-sonarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {
                    "name": "my-sonarr",
                    "namespace": "test",
                    "uid": "deploy-uid-1",
                    "resourceVersion": "100"
                },
                "spec": {
                    "selector": { "matchLabels": { "app": "my-sonarr" } },
                    "template": {
                        "metadata": { "labels": { "app": "my-sonarr" } },
                        "spec": { "containers": [{ "name": "sonarr", "image": "sonarr:latest" }] }
                    }
                },
                "status": {
                    "readyReplicas": 1,
                    "replicas": 1,
                    "availableReplicas": 1
                }
            })))
            .mount(&mock_server)
            .await;

        // Capture the PATCH status call to verify conditions
        Mock::given(method("PATCH"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/my-sonarr/status",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrApp",
                "metadata": {
                    "name": "my-sonarr",
                    "namespace": "test",
                    "uid": "sa-uid-1",
                    "resourceVersion": "200"
                },
                "spec": { "app": "Sonarr" }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let result = update_status(
            &client,
            &app,
            StatusConditions {
                health: None,
                update: None,
                admin_creds: None,
                bazarr_sync: None,
                subgen_sync: None,
                prowlarr_sync: None,
                seerr_sync: None,
                maintainerr_sync: None,
                restore: None,
                download_data: None,
            },
            None,
        )
        .await;
        assert!(
            result.is_ok(),
            "update_status should succeed, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn update_status_not_ready_deployment() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);

        // GET deployment returns readyReplicas=0
        Mock::given(method("GET"))
            .and(path("/apis/apps/v1/namespaces/test/deployments/my-sonarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {
                    "name": "my-sonarr",
                    "namespace": "test",
                    "uid": "deploy-uid-1",
                    "resourceVersion": "100"
                },
                "spec": {
                    "selector": { "matchLabels": { "app": "my-sonarr" } },
                    "template": {
                        "metadata": { "labels": { "app": "my-sonarr" } },
                        "spec": { "containers": [{ "name": "sonarr", "image": "sonarr:latest" }] }
                    }
                },
                "status": {
                    "readyReplicas": 0,
                    "replicas": 1,
                    "availableReplicas": 0
                }
            })))
            .mount(&mock_server)
            .await;

        // Capture and inspect the PATCH status call
        let status_mock = Mock::given(method("PATCH"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/my-sonarr/status",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrApp",
                "metadata": {
                    "name": "my-sonarr",
                    "namespace": "test",
                    "uid": "sa-uid-1",
                    "resourceVersion": "200"
                },
                "spec": { "app": "Sonarr" }
            })))
            .expect(1)
            .mount_as_scoped(&mock_server)
            .await;

        let result = update_status(
            &client,
            &app,
            StatusConditions {
                health: None,
                update: None,
                admin_creds: None,
                bazarr_sync: None,
                subgen_sync: None,
                prowlarr_sync: None,
                seerr_sync: None,
                maintainerr_sync: None,
                restore: None,
                download_data: None,
            },
            None,
        )
        .await;
        assert!(
            result.is_ok(),
            "update_status should succeed, got: {result:?}"
        );

        // Verify the PATCH was called (expect(1) will assert on drop)
        drop(status_mock);
    }

    // ---- discover_namespace_apps tests ----

    #[tokio::test]
    async fn discover_apps_finds_sonarr_radarr() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        // List ServarrApps returns Sonarr + Radarr with api_key_secret
        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrAppList",
                "metadata": {},
                "items": [
                    {
                        "apiVersion": "servarr.dev/v1alpha1",
                        "kind": "ServarrApp",
                        "metadata": {
                            "name": "my-sonarr",
                            "namespace": "test",
                            "uid": "sonarr-uid",
                            "resourceVersion": "1"
                        },
                        "spec": {
                            "app": "Sonarr",
                            "apiKeySecret": "sonarr-secret"
                        }
                    },
                    {
                        "apiVersion": "servarr.dev/v1alpha1",
                        "kind": "ServarrApp",
                        "metadata": {
                            "name": "my-radarr",
                            "namespace": "test",
                            "uid": "radarr-uid",
                            "resourceVersion": "1"
                        },
                        "spec": {
                            "app": "Radarr",
                            "apiKeySecret": "radarr-secret"
                        }
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        mount_secret_mock(
            &mock_server,
            "test",
            "sonarr-secret",
            json!({ "api-key": "c29uYXJyLWtleQ==" }),
        )
        .await;
        mount_secret_mock(
            &mock_server,
            "test",
            "radarr-secret",
            json!({ "api-key": "cmFkYXJyLWtleQ==" }),
        )
        .await;

        let result = discover_namespace_apps(&client, "test").await;
        assert!(
            result.is_ok(),
            "discover_namespace_apps should succeed, got: {result:?}"
        );
        let apps = result.unwrap();
        assert_eq!(apps.len(), 2, "should discover 2 apps");

        let sonarr = apps.iter().find(|a| a.name == "my-sonarr").unwrap();
        assert!(matches!(sonarr.app_type, AppType::Sonarr));
        assert_eq!(sonarr.api_key, "sonarr-key");

        let radarr = apps.iter().find(|a| a.name == "my-radarr").unwrap();
        assert!(matches!(radarr.app_type, AppType::Radarr));
        assert_eq!(radarr.api_key, "radarr-key");
    }

    #[tokio::test]
    async fn discover_apps_skips_transmission() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        // List ServarrApps returns Sonarr + Transmission
        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrAppList",
                "metadata": {},
                "items": [
                    {
                        "apiVersion": "servarr.dev/v1alpha1",
                        "kind": "ServarrApp",
                        "metadata": {
                            "name": "my-sonarr",
                            "namespace": "test",
                            "uid": "sonarr-uid",
                            "resourceVersion": "1"
                        },
                        "spec": {
                            "app": "Sonarr",
                            "apiKeySecret": "sonarr-secret"
                        }
                    },
                    {
                        "apiVersion": "servarr.dev/v1alpha1",
                        "kind": "ServarrApp",
                        "metadata": {
                            "name": "my-transmission",
                            "namespace": "test",
                            "uid": "tx-uid",
                            "resourceVersion": "1"
                        },
                        "spec": {
                            "app": "Transmission",
                            "apiKeySecret": "tx-secret"
                        }
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        mount_secret_mock(
            &mock_server,
            "test",
            "sonarr-secret",
            json!({ "api-key": "c29uYXJyLWtleQ==" }),
        )
        .await;

        let result = discover_namespace_apps(&client, "test").await;
        assert!(
            result.is_ok(),
            "discover_namespace_apps should succeed, got: {result:?}"
        );
        let apps = result.unwrap();
        assert_eq!(
            apps.len(),
            1,
            "should discover only 1 app (Transmission excluded)"
        );
        assert_eq!(apps[0].name, "my-sonarr");
        assert!(
            !apps.iter().any(|a| a.name == "my-transmission"),
            "Transmission should not be in discovered results"
        );
    }

    // ---- sync_maintainerr_servers Plex-wiring tests (#151, #251) ----
    //
    // These exercise sync_maintainerr_servers end to end: the ServarrApp list call
    // (shared by discover_namespace_apps and the separate Plex lookup), the
    // plex-token secret read, and the two Maintainerr calls (set_plex_token then
    // set_plex) that configure Plex. Secret data values are base64-encoded as
    // required by the kube Secret.data format:
    //   "my-plex-token" → "bXktcGxleC10b2tlbg=="

    async fn mount_servarrapps_list(mock_server: &MockServer, items: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrAppList",
                "metadata": {},
                "items": items
            })))
            .mount(mock_server)
            .await;
    }

    fn plex_app_json(name: &str) -> serde_json::Value {
        json!({
            "apiVersion": "servarr.dev/v1alpha1",
            "kind": "ServarrApp",
            "metadata": {
                "name": name,
                "namespace": "test",
                "uid": "plex-uid",
                "resourceVersion": "1"
            },
            "spec": { "app": "Plex" }
        })
    }

    async fn mount_maintainerr_list_mocks(m_server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/settings/sonarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(m_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/settings/radarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(m_server)
            .await;
    }

    fn maintainerr_app_with_plex_sync(plex_token_secret: Option<&str>) -> ServarrApp {
        let mut app = make_test_app("my-maintainerr", "test", AppType::Maintainerr);
        app.spec.maintainerr_sync = Some(servarr_crds::MaintainerrSyncSpec {
            enabled: true,
            namespace_scope: None,
            plex_token_secret: plex_token_secret.map(str::to_string),
        });
        app
    }

    async fn mount_maintainerr_api_key_secret(mock_server: &MockServer, maintainerr: &ServarrApp) {
        mount_secret_mock(
            mock_server,
            "test",
            &servarr_resources::common::child_name(maintainerr, "api-key"),
            json!({ "api-key": "bWFpbnRhaW5lcnIta2V5" }),
        )
        .await;
    }

    #[tokio::test]
    async fn sync_maintainerr_configures_plex_via_servarrapp_lookup() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        let m_server = MockServer::start().await;

        let maintainerr = maintainerr_app_with_plex_sync(Some("plex-token-secret"));
        let port = servarr_crds::AppDefaults::for_app(&AppType::Plex)
            .expect("Plex defaults")
            .service
            .ports[0]
            .port as u16;

        mount_maintainerr_api_key_secret(&mock_server, &maintainerr).await;
        mount_secret_mock(
            &mock_server,
            "test",
            "plex-token-secret",
            json!({ "plex-token": "bXktcGxleC10b2tlbg==" }),
        )
        .await;
        mount_servarrapps_list(&mock_server, json!([plex_app_json("my-plex")])).await;
        mount_maintainerr_list_mocks(&m_server).await;

        Mock::given(method("POST"))
            .and(path("/api/settings/plex/token"))
            .and(body_json(json!({ "plex_auth_token": "my-plex-token" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&m_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/settings"))
            .and(body_json(json!({
                "plex_hostname": "my-plex.test.svc",
                "plex_port": port,
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&m_server)
            .await;

        let result =
            sync_maintainerr_servers(&client, &maintainerr, "test", Some(&m_server.uri())).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn sync_maintainerr_no_plex_token_secret_skips_plex() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        let m_server = MockServer::start().await;

        let maintainerr = maintainerr_app_with_plex_sync(None);
        mount_maintainerr_api_key_secret(&mock_server, &maintainerr).await;
        mount_servarrapps_list(&mock_server, json!([plex_app_json("my-plex")])).await;
        mount_maintainerr_list_mocks(&m_server).await;

        // No plex-token-secret mock mounted, and no Plex endpoints mounted on
        // m_server: reading the secret and calling Plex endpoints must never be
        // attempted when plex_token_secret is None.
        let result =
            sync_maintainerr_servers(&client, &maintainerr, "test", Some(&m_server.uri())).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn sync_maintainerr_missing_plex_token_key_skips_plex_without_failure() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        let m_server = MockServer::start().await;

        let maintainerr = maintainerr_app_with_plex_sync(Some("plex-token-secret"));
        mount_maintainerr_api_key_secret(&mock_server, &maintainerr).await;
        // Secret exists but lacks the plex-token key.
        mount_secret_mock(&mock_server, "test", "plex-token-secret", json!({})).await;
        mount_servarrapps_list(&mock_server, json!([plex_app_json("my-plex")])).await;
        mount_maintainerr_list_mocks(&m_server).await;

        // A missing key is logged and treated as "Plex not configured", not a
        // sync failure — matches the read_secret_key contract used everywhere else.
        let result =
            sync_maintainerr_servers(&client, &maintainerr, "test", Some(&m_server.uri())).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn sync_maintainerr_plex_token_secret_api_error_counts_as_failure() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        let m_server = MockServer::start().await;

        let maintainerr = maintainerr_app_with_plex_sync(Some("plex-token-secret"));
        mount_maintainerr_api_key_secret(&mock_server, &maintainerr).await;
        // Secret read fails with a non-404 K8s API error (e.g. RBAC denial).
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/plex-token-secret"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {},
                "status": "Failure",
                "message": "secrets \"plex-token-secret\" is forbidden",
                "reason": "Forbidden",
                "code": 403
            })))
            .mount(&mock_server)
            .await;
        mount_servarrapps_list(&mock_server, json!([plex_app_json("my-plex")])).await;
        mount_maintainerr_list_mocks(&m_server).await;

        // A non-404 Kubernetes API error (permission denied, transient failure) is an
        // infrastructure problem, not an intentional skip — it must count as a sync
        // failure so the controller retries with backoff (#253).
        let result =
            sync_maintainerr_servers(&client, &maintainerr, "test", Some(&m_server.uri())).await;
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    #[tokio::test]
    async fn sync_maintainerr_no_plex_servarrapp_skips_plex() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        let m_server = MockServer::start().await;

        let maintainerr = maintainerr_app_with_plex_sync(Some("plex-token-secret"));
        mount_maintainerr_api_key_secret(&mock_server, &maintainerr).await;
        mount_secret_mock(
            &mock_server,
            "test",
            "plex-token-secret",
            json!({ "plex-token": "bXktcGxleC10b2tlbg==" }),
        )
        .await;
        // No Plex ServarrApp in the namespace — token is present but there's
        // nothing to configure it against.
        mount_servarrapps_list(&mock_server, json!([])).await;
        mount_maintainerr_list_mocks(&m_server).await;

        let result =
            sync_maintainerr_servers(&client, &maintainerr, "test", Some(&m_server.uri())).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn sync_maintainerr_set_plex_token_failure_counts_as_sync_failure() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        let m_server = MockServer::start().await;

        let maintainerr = maintainerr_app_with_plex_sync(Some("plex-token-secret"));
        mount_maintainerr_api_key_secret(&mock_server, &maintainerr).await;
        mount_secret_mock(
            &mock_server,
            "test",
            "plex-token-secret",
            json!({ "plex-token": "bXktcGxleC10b2tlbg==" }),
        )
        .await;
        mount_servarrapps_list(&mock_server, json!([plex_app_json("my-plex")])).await;
        mount_maintainerr_list_mocks(&m_server).await;

        Mock::given(method("POST"))
            .and(path("/api/settings/plex/token"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&m_server)
            .await;

        let result =
            sync_maintainerr_servers(&client, &maintainerr, "test", Some(&m_server.uri())).await;
        assert!(
            result.is_err(),
            "a failed Plex token call must surface as a sync failure"
        );
    }

    // ---- #428-430 pattern check: kube::Error::Api still keeps its status code ----

    /// For the `Api` variant specifically, `kube_err_summary` and `kube_err_public_summary`
    /// collapse to the same status-code string — this guards that equivalence, not the
    /// log-only-vs-tenant-safe distinction (see the `#437` tests below for that).
    #[tokio::test]
    async fn discover_namespace_apps_list_error_keeps_only_the_status_code() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {},
                "status": "Failure",
                "message": "servarrapps.servarr.dev is forbidden: User \
                            \"system:serviceaccount:kube-system:leak-marker-sa\" cannot list",
                "reason": "Forbidden",
                "code": 403
            })))
            .mount(&mock_server)
            .await;

        let err = discover_namespace_apps(&client, "test")
            .await
            .expect_err("a 403 on the ServarrApp list must surface as an error")
            .to_string();

        assert!(err.contains("403"), "should keep the status code: {err}");
        assert!(
            !err.contains("leak-marker-sa"),
            "must not leak the API server's RBAC message: {err}"
        );
    }

    // ---- #437: tenant-visible kube::Error/SecretError sanitization ----

    /// A non-`Api` `kube::Error` is the only thing that tells `kube_err_summary` (log-safe) apart
    /// from `kube_err_public_summary` (tenant-safe): for `Api` both collapse to the same
    /// status-code string, but for every other variant the log-safe one passes `Display` through
    /// verbatim. A 200 whose body has the wrong shape yields a deserialization error whose
    /// `Display` embeds the offending value, so this marker reaches the caller if and only if the
    /// call site used the log-only sanitizer. The same applies to `SecretError::Kube`, which just
    /// delegates to the same two kube functions.
    const SERDE_LEAK_MARKER: &str = "leak-marker-bearer-tok-abc123";

    #[tokio::test]
    async fn discover_namespace_apps_list_error_collapses_non_api_kube_errors() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrAppList",
                "metadata": {},
                "items": SERDE_LEAK_MARKER
            })))
            .mount(&mock_server)
            .await;

        let err = discover_namespace_apps(&client, "test")
            .await
            .expect_err("a malformed list body must surface as an error")
            .to_string();

        assert!(
            err.contains("failed to list ServarrApps"),
            "should keep the call-site context: {err}"
        );
        assert!(
            !err.contains(SERDE_LEAK_MARKER),
            "must not pass a non-Api kube::Error's Display through to the tenant: {err}"
        );
    }

    #[tokio::test]
    async fn sync_prowlarr_apps_api_key_read_error_is_tenant_sanitized() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let mut prowlarr = make_test_app("my-prowlarr", "test", AppType::Prowlarr);
        prowlarr.spec.api_key_secret = Some("prowlarr-api-key".into());

        mount_secret_mock(
            &mock_server,
            "test",
            "prowlarr-api-key",
            json!(SERDE_LEAK_MARKER),
        )
        .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = prowlarr.object_ref(&());

        let err = sync_prowlarr_apps(&client, &prowlarr, "test", &recorder, &obj_ref)
            .await
            .expect_err("an unreadable API-key secret must surface as an error")
            .to_string();

        assert!(
            err.contains("failed to read Prowlarr API key"),
            "should keep the call-site context: {err}"
        );
        assert!(
            !err.contains(SERDE_LEAK_MARKER),
            "must not pass a SecretError::Kube wrapping a non-Api kube::Error through to the tenant: {err}"
        );
    }

    #[tokio::test]
    async fn sync_seerr_servers_api_key_read_error_is_tenant_sanitized() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let mut seerr = make_test_app("my-seerr", "test", AppType::Seerr);
        seerr.spec.api_key_secret = Some("seerr-api-key".into());

        mount_secret_mock(
            &mock_server,
            "test",
            "seerr-api-key",
            json!(SERDE_LEAK_MARKER),
        )
        .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = seerr.object_ref(&());

        let err = sync_seerr_servers(&client, &seerr, "test", &recorder, &obj_ref)
            .await
            .expect_err("an unreadable API-key secret must surface as an error")
            .to_string();

        assert!(
            err.contains("failed to read Seerr API key"),
            "should keep the call-site context: {err}"
        );
        assert!(
            !err.contains(SERDE_LEAK_MARKER),
            "must not pass a SecretError::Kube wrapping a non-Api kube::Error through to the tenant: {err}"
        );
    }

    #[tokio::test]
    async fn sync_bazarr_apps_api_key_read_error_is_tenant_sanitized() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let bazarr = make_test_app("my-bazarr", "test", AppType::Bazarr);
        let secret_name = servarr_resources::common::child_name(&bazarr, "api-key");

        mount_secret_mock(&mock_server, "test", &secret_name, json!(SERDE_LEAK_MARKER)).await;

        let err = sync_bazarr_apps(&client, &bazarr, "test")
            .await
            .expect_err("an unreadable API-key secret must surface as an error")
            .to_string();

        assert!(
            err.contains("failed to read Bazarr API key"),
            "should keep the call-site context: {err}"
        );
        assert!(
            !err.contains(SERDE_LEAK_MARKER),
            "must not pass a SecretError::Kube wrapping a non-Api kube::Error through to the tenant: {err}"
        );
    }

    #[tokio::test]
    async fn sync_maintainerr_servers_api_key_read_error_is_tenant_sanitized() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let maintainerr = make_test_app("my-maintainerr-2", "test", AppType::Maintainerr);
        let secret_name = servarr_resources::common::child_name(&maintainerr, "api-key");

        mount_secret_mock(&mock_server, "test", &secret_name, json!(SERDE_LEAK_MARKER)).await;

        let err = sync_maintainerr_servers(&client, &maintainerr, "test", None)
            .await
            .expect_err("an unreadable API-key secret must surface as an error")
            .to_string();

        assert!(
            err.contains("failed to read Maintainerr API key"),
            "should keep the call-site context: {err}"
        );
        assert!(
            !err.contains(SERDE_LEAK_MARKER),
            "must not pass a SecretError::Kube wrapping a non-Api kube::Error through to the tenant: {err}"
        );
    }

    #[tokio::test]
    async fn sync_subgen_jellyfin_list_error_collapses_non_api_kube_errors() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let subgen = make_test_app("my-subgen", "test", AppType::Subgen);

        Mock::given(method("GET"))
            .and(path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrAppList",
                "metadata": {},
                "items": SERDE_LEAK_MARKER
            })))
            .mount(&mock_server)
            .await;

        let err = sync_subgen_jellyfin(&client, &subgen, "test")
            .await
            .expect_err("a malformed list body must surface as an error")
            .to_string();

        assert!(
            err.contains("failed to list ServarrApps"),
            "should keep the call-site context: {err}"
        );
        assert!(
            !err.contains(SERDE_LEAK_MARKER),
            "must not pass a non-Api kube::Error's Display through to the tenant: {err}"
        );
    }

    #[tokio::test]
    async fn try_restore_api_key_read_error_is_tenant_sanitized() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let mut app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        app.spec.api_key_secret = Some("sonarr-api-key".into());

        mount_secret_mock(
            &mock_server,
            "test",
            "sonarr-api-key",
            json!(SERDE_LEAK_MARKER),
        )
        .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let err = try_restore(&client, &app, 1, &recorder, &obj_ref)
            .await
            .expect_err("an unreadable API-key secret must surface as an error")
            .to_string();

        assert!(
            err.contains("failed to read API key for restore"),
            "should keep the call-site context: {err}"
        );
        assert!(
            !err.contains(SERDE_LEAK_MARKER),
            "must not pass a SecretError::Kube wrapping a non-Api kube::Error through to the tenant: {err}"
        );
    }

    #[tokio::test]
    async fn maybe_run_backup_secret_read_error_sanitizes_backup_status() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;

        let mut app = make_test_app("my-sonarr-backup", "test", AppType::Sonarr);
        app.spec.api_key_secret = Some("sonarr-backup-api-key".into());
        app.spec.backup = Some(servarr_crds::BackupSpec {
            enabled: true,
            schedule: "0 3 * * *".into(),
            retention_count: 5,
        });

        mount_secret_mock(
            &mock_server,
            "test",
            "sonarr-backup-api-key",
            json!(SERDE_LEAK_MARKER),
        )
        .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let status = maybe_run_backup(&client, &app, &recorder, &obj_ref)
            .await
            .expect("a secret-read failure still yields a BackupStatus");
        let result = status
            .last_backup_result
            .expect("failure path always sets last_backup_result");

        assert!(
            result.contains("secret read error"),
            "should keep the call-site context: {result}"
        );
        assert!(
            !result.contains(SERDE_LEAK_MARKER),
            "must not pass a SecretError::Kube wrapping a non-Api kube::Error through to the tenant-visible status.backupStatus.lastBackupResult: {result}"
        );
    }

    // ---- finalizer cleanup CleanupFailed Event tests (#444) ----

    /// Seed used to prove the Warning Event message is tenant-safe: it can never
    /// appear in a sanitized summary, so its absence proves the raw message did
    /// not reach the Event.
    const CLEANUP_FAILED_SEED: &str = "CLEANUP-SEED-SECRET-TOKEN";

    /// Collect the bodies of all Event POSTs to the kube MockServer so tests can
    /// assert on the published Event (type, reason, note).
    async fn event_post_bodies(mock_server: &MockServer) -> Vec<serde_json::Value> {
        let mut bodies = Vec::new();
        for req in mock_server.received_requests().await.unwrap_or_default() {
            if req.method == wiremock::http::Method::POST
                && req.url.path() == "/apis/events.k8s.io/v1/namespaces/test/events"
                && let Ok(body) = serde_json::from_slice::<serde_json::Value>(&req.body)
            {
                bodies.push(body);
            }
        }
        bodies
    }

    /// Answer Event POSTs with a minimal events.k8s.io/v1 Event.
    async fn mount_event_post_mock(mock_server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion": "events.k8s.io/v1",
                "kind": "Event",
                "metadata": {
                    "name": "test-event",
                    "namespace": "test",
                    "uid": "event-uid-1",
                    "resourceVersion": "300"
                }
            })))
            .mount(mock_server)
            .await;
    }

    /// Answer a GET with a `kube::Error::Api` (a `Status` whose message carries
    /// `seed`) so the tenant-safe summary keeps only the status code.
    async fn mount_kube_status_error(
        mock_server: &MockServer,
        url_path: &str,
        code: u16,
        seed: &str,
    ) {
        Mock::given(method("GET"))
            .and(path(url_path))
            .respond_with(ResponseTemplate::new(code).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Status",
                "code": code,
                "message": format!("{url_path} is forbidden: {seed}"),
                "reason": "Forbidden"
            })))
            .mount(mock_server)
            .await;
    }

    /// A sync-enabled Prowlarr `ServarrApp` for a ServarrAppList, with an optional
    /// `apiKeySecret`.
    fn prowlarr_app_json(name: &str, api_key_secret: Option<&str>) -> serde_json::Value {
        let mut spec = serde_json::Map::new();
        spec.insert("app".into(), json!("Prowlarr"));
        spec.insert("prowlarrSync".into(), json!({ "enabled": true }));
        if let Some(secret) = api_key_secret {
            spec.insert("apiKeySecret".into(), json!(secret));
        }
        json!({
            "apiVersion": "servarr.dev/v1alpha1",
            "kind": "ServarrApp",
            "metadata": {
                "name": name,
                "namespace": "test",
                "uid": "prowlarr-uid",
                "resourceVersion": "1"
            },
            "spec": spec
        })
    }

    /// A sync-enabled Seerr `ServarrApp` for a ServarrAppList, with an optional
    /// `apiKeySecret`.
    fn seerr_app_json(name: &str, api_key_secret: Option<&str>) -> serde_json::Value {
        let mut spec = serde_json::Map::new();
        spec.insert("app".into(), json!("Seerr"));
        spec.insert("seerrSync".into(), json!({ "enabled": true }));
        if let Some(secret) = api_key_secret {
            spec.insert("apiKeySecret".into(), json!(secret));
        }
        json!({
            "apiVersion": "servarr.dev/v1alpha1",
            "kind": "ServarrApp",
            "metadata": {
                "name": name,
                "namespace": "test",
                "uid": "seerr-uid",
                "resourceVersion": "1"
            },
            "spec": spec
        })
    }

    /// A `kube::Error::Api` from the ServarrApp list call must emit exactly one
    /// `Warning`/`CleanupFailed` Event whose note is the tenant-safe status summary
    /// (not the seeded API-server message).
    #[tokio::test]
    async fn cleanup_prowlarr_registration_list_failure_emits_cleanup_failed_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        mount_kube_status_error(
            &mock_server,
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            403,
            CLEANUP_FAILED_SEED,
        )
        .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        let obj_ref = app.object_ref(&());

        let err = cleanup_prowlarr_registration(&client, &app, "test", &recorder, &obj_ref)
            .await
            .expect_err("a failed ServarrApp list must surface as an error");

        assert!(
            err.to_string().contains("failed to list ServarrApps"),
            "keep the call-site context: {err}"
        );

        let events = event_post_bodies(&mock_server).await;
        assert_eq!(events.len(), 1, "exactly one Event on failure: {events:?}");
        let event = &events[0];
        assert_eq!(event["type"], "Warning");
        assert_eq!(event["reason"], "CleanupFailed");
        assert_eq!(event["note"], "Kubernetes API error (status: 403)");
        let note = event["note"].as_str().expect("note is a string");
        assert!(
            !note.contains(CLEANUP_FAILED_SEED),
            "Event note must be tenant-safe, got: {note}"
        );
    }

    /// No sync-enabled Prowlarr in the namespace is a skip: Ok and no Event.
    #[tokio::test]
    async fn cleanup_prowlarr_registration_no_sync_publishes_no_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        mount_servarrapps_list(&mock_server, json!([])).await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        let obj_ref = app.object_ref(&());

        cleanup_prowlarr_registration(&client, &app, "test", &recorder, &obj_ref)
            .await
            .expect("no sync-enabled Prowlarr is a no-op success");

        assert!(
            event_post_bodies(&mock_server).await.is_empty(),
            "skip path must not publish any Event"
        );
    }

    /// A sync-enabled Prowlarr whose registered apps do not include this app's baseUrl is a
    /// skip: Ok and no Event.
    #[tokio::test]
    async fn cleanup_prowlarr_registration_no_matching_server_publishes_no_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        mount_servarrapps_list(
            &mock_server,
            json!([prowlarr_app_json("my-prowlarr", Some("prowlarr-secret"))]),
        )
        .await;
        mount_secret_mock(
            &mock_server,
            "test",
            "prowlarr-secret",
            json!({ "api-key": "dGVzdC1rZXk=" }),
        )
        .await;

        let p_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/applications"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "id": 1,
                    "name": "other",
                    "fields": [{ "name": "baseUrl", "value": "http://other.svc:80" }]
                }
            ])))
            .mount(&p_server)
            .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        cleanup_prowlarr_registration_body(
            &client,
            &app,
            "test",
            &recorder,
            &obj_ref,
            Some(&p_server.uri()),
        )
        .await
        .expect("no matching registered application is a no-op success");

        assert!(
            event_post_bodies(&mock_server).await.is_empty(),
            "no-match skip path must not publish any Event"
        );
    }

    /// A `SecretError::Kube` from the Seerr API-key read must emit exactly one
    /// `Warning`/`CleanupFailed` Event with the tenant-safe status summary.
    #[tokio::test]
    async fn cleanup_seerr_registration_secret_read_failure_emits_cleanup_failed_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        mount_servarrapps_list(
            &mock_server,
            json!([seerr_app_json("my-seerr", Some("seerr-secret"))]),
        )
        .await;
        mount_kube_status_error(
            &mock_server,
            "/api/v1/namespaces/test/secrets/seerr-secret",
            403,
            CLEANUP_FAILED_SEED,
        )
        .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        let obj_ref = app.object_ref(&());

        let err = cleanup_seerr_registration(&client, &app, "test", &recorder, &obj_ref)
            .await
            .expect_err("a failed secret read must surface as an error");

        assert!(
            err.to_string().contains("failed to read Seerr API key"),
            "keep the call-site context: {err}"
        );

        let events = event_post_bodies(&mock_server).await;
        assert_eq!(events.len(), 1, "exactly one Event on failure: {events:?}");
        let event = &events[0];
        assert_eq!(event["type"], "Warning");
        assert_eq!(event["reason"], "CleanupFailed");
        assert_eq!(event["note"], "Kubernetes API error (status: 403)");
        let note = event["note"].as_str().expect("note is a string");
        assert!(
            !note.contains(CLEANUP_FAILED_SEED),
            "Event note must be tenant-safe, got: {note}"
        );
    }

    /// A sync-enabled Seerr with no `apiKeySecret` is a skip: Ok and no Event.
    #[tokio::test]
    async fn cleanup_seerr_registration_no_api_key_secret_publishes_no_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        mount_servarrapps_list(&mock_server, json!([seerr_app_json("my-seerr", None)])).await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        let obj_ref = app.object_ref(&());

        cleanup_seerr_registration(&client, &app, "test", &recorder, &obj_ref)
            .await
            .expect("an Seerr with no apiKeySecret is a no-op success");

        assert!(
            event_post_bodies(&mock_server).await.is_empty(),
            "skip path must not publish any Event"
        );
    }

    /// A sync-enabled Seerr whose registered Sonarr servers do not include this app's
    /// hostname+port is a skip: Ok and no Event.
    #[tokio::test]
    async fn cleanup_seerr_registration_no_matching_server_publishes_no_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        mount_servarrapps_list(
            &mock_server,
            json!([seerr_app_json("my-seerr", Some("seerr-secret"))]),
        )
        .await;
        mount_secret_mock(
            &mock_server,
            "test",
            "seerr-secret",
            json!({ "api-key": "dGVzdC1rZXk=" }),
        )
        .await;

        let o_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/settings/sonarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": 1,
                "name": "Other",
                "hostname": "other.test.svc",
                "port": 9999,
                "apiKey": "some-key",
                "useSsl": false,
                "activeProfileId": 1,
                "activeProfileName": "HD",
                "activeDirectory": "/media",
                "is4k": false,
                "enableSeasonFolders": true,
                "isDefault": true
            }])))
            .mount(&o_server)
            .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        cleanup_seerr_registration_body(
            &client,
            &app,
            "test",
            &recorder,
            &obj_ref,
            Some(&o_server.uri()),
        )
        .await
        .expect("no matching registered Sonarr server is a no-op success");

        assert!(
            event_post_bodies(&mock_server).await.is_empty(),
            "no-match skip path must not publish any Event"
        );
    }

    /// Successful removal publishes the existing `Normal`/`ProwlarrCleanup` Event and
    /// never a `CleanupFailed` one. Exercised through `_body` with a `base_url_override`
    /// because the in-cluster Prowlarr URL cannot resolve in tests.
    #[tokio::test]
    async fn cleanup_prowlarr_registration_success_publishes_normal_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        let port = servarr_crds::AppDefaults::for_app(&AppType::Sonarr)
            .expect("Sonarr defaults")
            .service
            .ports[0]
            .port;
        let app_url = format!("http://{}.test.svc:{port}", app.name_any());

        mount_servarrapps_list(
            &mock_server,
            json!([prowlarr_app_json("my-prowlarr", Some("prowlarr-secret"))]),
        )
        .await;
        mount_secret_mock(
            &mock_server,
            "test",
            "prowlarr-secret",
            json!({ "api-key": "dGVzdC1rZXk=" }),
        )
        .await;

        let p_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/applications"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 1, "name": "sonarr", "fields": [{ "name": "baseUrl", "value": app_url }] }
            ])))
            .mount(&p_server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/applications/1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&p_server)
            .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        cleanup_prowlarr_registration_body(
            &client,
            &app,
            "test",
            &recorder,
            &obj_ref,
            Some(&p_server.uri()),
        )
        .await
        .expect("a matching registered application is removed");

        let events = event_post_bodies(&mock_server).await;
        assert_eq!(
            events.len(),
            1,
            "exactly one Normal Event on success: {events:?}"
        );
        assert_eq!(events[0]["type"], "Normal");
        assert_eq!(events[0]["reason"], "ProwlarrCleanup");
    }

    /// Successful removal publishes the existing `Normal`/`SeerrCleanup` Event and
    /// never a `CleanupFailed` one. Exercised through `_body` with a `base_url_override`.
    #[tokio::test]
    async fn cleanup_seerr_registration_success_publishes_normal_event() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        let port = servarr_crds::AppDefaults::for_app(&AppType::Sonarr)
            .expect("Sonarr defaults")
            .service
            .ports[0]
            .port;
        let app_hostname = format!("{}.test.svc", app.name_any());

        mount_servarrapps_list(
            &mock_server,
            json!([seerr_app_json("my-seerr", Some("seerr-secret"))]),
        )
        .await;
        mount_secret_mock(
            &mock_server,
            "test",
            "seerr-secret",
            json!({ "api-key": "dGVzdC1rZXk=" }),
        )
        .await;

        let o_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/settings/sonarr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": 1,
                "name": "Sonarr",
                "hostname": app_hostname,
                "port": port,
                "apiKey": "some-key",
                "useSsl": false,
                "activeProfileId": 1,
                "activeProfileName": "HD",
                "activeDirectory": "/media",
                "is4k": false,
                "enableSeasonFolders": true,
                "isDefault": true
            }])))
            .mount(&o_server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/settings/sonarr/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "name": "Sonarr",
                "hostname": app_hostname,
                "port": port,
                "apiKey": "some-key",
                "useSsl": false,
                "activeProfileId": 1,
                "activeProfileName": "HD",
                "activeDirectory": "/media",
                "is4k": false,
                "enableSeasonFolders": true,
                "isDefault": true
            })))
            .expect(1)
            .mount(&o_server)
            .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        cleanup_seerr_registration_body(
            &client,
            &app,
            "test",
            &recorder,
            &obj_ref,
            Some(&o_server.uri()),
        )
        .await
        .expect("a matching registered Sonarr server is removed");

        let events = event_post_bodies(&mock_server).await;
        assert_eq!(
            events.len(),
            1,
            "exactly one Normal Event on success: {events:?}"
        );
        assert_eq!(events[0]["type"], "Normal");
        assert_eq!(events[0]["reason"], "SeerrCleanup");
    }

    // ---- CleanupSeverity classification tests (#451) ----

    fn api_status_error(code: u16) -> kube::Error {
        kube::Error::Api(Box::new(kube::core::Status {
            code,
            ..Default::default()
        }))
    }

    #[test]
    fn kube_error_cleanup_severity_by_status_code() {
        for (code, expected) in [
            (404, CleanupSeverity::Terminal),
            (400, CleanupSeverity::Transient),
            (403, CleanupSeverity::Transient),
            (409, CleanupSeverity::Transient),
            (500, CleanupSeverity::Transient),
            (503, CleanupSeverity::Transient),
        ] {
            assert_eq!(
                api_status_error(code).cleanup_severity(),
                expected,
                "status {code} should be {expected:?}"
            );
        }
    }

    #[test]
    fn secret_error_kube_cleanup_severity_by_status_code() {
        for (code, expected) in [
            (404, CleanupSeverity::Terminal),
            (403, CleanupSeverity::Transient),
        ] {
            let err = servarr_api::SecretError::Kube(api_status_error(code));
            assert_eq!(
                err.cleanup_severity(),
                expected,
                "status {code} should be {expected:?}"
            );
        }
    }

    #[test]
    fn secret_error_malformed_secret_is_transient() {
        // The Secret exists but is missing data/the key, or the value isn't UTF-8 — that's a
        // config problem, not proof the downstream state is absent, so it must keep retrying.
        let errs = [
            servarr_api::SecretError::NoData { name: "s".into() },
            servarr_api::SecretError::KeyNotFound {
                name: "s".into(),
                key: "k".into(),
            },
            servarr_api::SecretError::InvalidUtf8 {
                name: "s".into(),
                key: "k".into(),
            },
        ];
        for err in errs {
            assert_eq!(
                err.cleanup_severity(),
                CleanupSeverity::Transient,
                "{err:?} should be transient"
            );
        }
    }

    #[test]
    fn api_error_cleanup_severity_by_status_code() {
        for (status, expected) in [
            (404, CleanupSeverity::Terminal),
            (400, CleanupSeverity::Transient),
            (401, CleanupSeverity::Transient),
            (403, CleanupSeverity::Transient),
            (500, CleanupSeverity::Transient),
            (503, CleanupSeverity::Transient),
        ] {
            let err = servarr_api::ApiError::ApiResponse {
                status,
                body: String::new(),
            };
            assert_eq!(
                err.cleanup_severity(),
                expected,
                "status {status} should be {expected:?}"
            );
        }
    }

    // ---- CleanupSeverity::Terminal wrapper behavior tests (#451) ----

    /// A 404 on the `api_key_secret` Secret GET (deleted out-of-band) is terminal for either
    /// cleanup path: the wrapper must treat it as idempotent success — `Ok(())`, no
    /// `CleanupFailed` Event — instead of a retryable failure. Generates one test per
    /// (cleanup fn, app-json builder, app name, secret name) tuple; the two invocations below
    /// differ only in those four identifiers.
    macro_rules! secret_not_found_is_terminal_no_event_test {
        ($test_name:ident, $cleanup_fn:ident, $app_json_fn:ident, $app_name:literal, $secret_name:literal) => {
            #[tokio::test]
            async fn $test_name() {
                let mock_server = MockServer::start().await;
                let client = build_mock_client(&mock_server.uri()).await;
                mount_event_post_mock(&mock_server).await;
                mount_servarrapps_list(
                    &mock_server,
                    json!([$app_json_fn($app_name, Some($secret_name))]),
                )
                .await;
                mount_kube_status_error(
                    &mock_server,
                    concat!("/api/v1/namespaces/test/secrets/", $secret_name),
                    404,
                    "unused-seed",
                )
                .await;

                let recorder = Recorder::new(client.clone(), "test".into());
                let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
                let obj_ref = app.object_ref(&());

                $cleanup_fn(&client, &app, "test", &recorder, &obj_ref)
                    .await
                    .expect("a secret deleted out-of-band is terminal, not a failure");

                assert!(
                    event_post_bodies(&mock_server).await.is_empty(),
                    "terminal cleanup must not publish a CleanupFailed Event"
                );
            }
        };
    }

    secret_not_found_is_terminal_no_event_test!(
        cleanup_prowlarr_registration_secret_not_found_is_terminal_no_event,
        cleanup_prowlarr_registration,
        prowlarr_app_json,
        "my-prowlarr",
        "prowlarr-secret"
    );
    secret_not_found_is_terminal_no_event_test!(
        cleanup_seerr_registration_secret_not_found_is_terminal_no_event,
        cleanup_seerr_registration,
        seerr_app_json,
        "my-seerr",
        "seerr-secret"
    );

    // ---- CleanupSeverity::Transient LIST-endpoint tests (#451 review follow-up) ----
    //
    // A 404 on a GET-by-name/DELETE-by-id call proves the specific target is gone (terminal).
    // A 404 on a LIST/collection call means the *endpoint* wasn't found (CRD not yet served,
    // misconfigured urlBase, wrong route) — a real, retryable problem, not proof the cleanup
    // target is absent. `cleanup_map_err_transient` must be used at every LIST call site so this
    // never folds into the terminal/idempotent-success path.

    /// A 404 from the ServarrApps LIST call itself (used to find the sync-enabled Prowlarr
    /// instance) must stay Transient: publish `CleanupFailed` and return `Err`, not silently
    /// succeed.
    #[tokio::test]
    async fn cleanup_prowlarr_registration_servarrapps_list_404_is_transient_not_terminal() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        mount_kube_status_error(
            &mock_server,
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            404,
            "unused-seed",
        )
        .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        let obj_ref = app.object_ref(&());

        cleanup_prowlarr_registration(&client, &app, "test", &recorder, &obj_ref)
            .await
            .expect_err("a 404 on a LIST call is transient, not proof the target is gone");

        let events = event_post_bodies(&mock_server).await;
        assert_eq!(
            events.len(),
            1,
            "a transient LIST failure must still publish CleanupFailed: {events:?}"
        );
        assert_eq!(events[0]["reason"], "CleanupFailed");
    }

    /// A 404 from Prowlarr's own `list_applications` call must stay Transient for the same
    /// reason — it's a collection endpoint, not a lookup of a specific registration.
    #[tokio::test]
    async fn cleanup_prowlarr_registration_list_applications_404_is_transient_not_terminal() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;

        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);
        mount_servarrapps_list(
            &mock_server,
            json!([prowlarr_app_json("my-prowlarr", Some("prowlarr-secret"))]),
        )
        .await;
        mount_secret_mock(
            &mock_server,
            "test",
            "prowlarr-secret",
            json!({ "api-key": "dGVzdC1rZXk=" }),
        )
        .await;

        let p_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/applications"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "message": "not found"
            })))
            .mount(&p_server)
            .await;

        let recorder = Recorder::new(client.clone(), "test".into());
        let obj_ref = app.object_ref(&());

        let err = cleanup_prowlarr_registration_body(
            &client,
            &app,
            "test",
            &recorder,
            &obj_ref,
            Some(&p_server.uri()),
        )
        .await
        .expect_err("a 404 on Prowlarr's applications LIST is transient, not proof of absence");

        assert_eq!(
            err.severity,
            CleanupSeverity::Transient,
            "LIST-endpoint 404 must not classify as Terminal"
        );
    }

    /// The same LIST-vs-lookup distinction applies to Seerr's `list_sonarr`/`list_radarr`.
    #[tokio::test]
    async fn seerr_remove_server_list_404_is_transient_not_terminal() {
        let o_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/settings/sonarr"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "message": "not found"
            })))
            .mount(&o_server)
            .await;

        let seerr_client = servarr_api::SeerrClient::new(&o_server.uri(), "test-key");
        let app = make_test_app("my-sonarr", "test", AppType::Sonarr);

        let err = seerr_remove_server(&seerr_client, &app, "my-sonarr.test.svc", 8989, SonarrSeerr)
            .await
            .expect_err("a 404 on Seerr's Sonarr-servers LIST is transient, not proof of absence");

        assert_eq!(
            err.severity,
            CleanupSeverity::Transient,
            "LIST-endpoint 404 must not classify as Terminal"
        );
    }

    // ---- reconcile() finalizer-retry behavior tests (#451) ----
    //
    // Before #451, `reconcile()` unconditionally stripped both finalizers after a deleting app's
    // cleanup attempts, regardless of whether either cleanup actually succeeded — a transient
    // failure (e.g. a listing call returning 500) silently orphaned the downstream registration
    // instead of ever being retried. These tests exercise `reconcile()` itself (not just the
    // cleanup wrapper) to prove the finalizer is now retained, and an error returned so
    // `error_policy` requeues, when cleanup is merely transient — and that it's still dropped
    // when cleanup proves the downstream target is already gone.

    /// Build a deleting Sonarr `ServarrApp` carrying both cleanup finalizers.
    fn make_deleting_app_with_finalizers(name: &str, ns: &str) -> ServarrApp {
        let mut app = make_test_app(name, ns, AppType::Sonarr);
        app.metadata.deletion_timestamp =
            Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::now(),
            ));
        app.metadata.finalizers = Some(vec![
            PROWLARR_FINALIZER.to_string(),
            SEERR_FINALIZER.to_string(),
        ]);
        app
    }

    /// Capture the JSON bodies of PATCH requests to the ServarrApp's own (non-status) endpoint,
    /// i.e. the finalizer-removal patch `reconcile()` issues.
    async fn servarrapp_patch_bodies(
        mock_server: &MockServer,
        name: &str,
    ) -> Vec<serde_json::Value> {
        let mut bodies = Vec::new();
        let expected_path =
            format!("/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/{name}");
        for req in mock_server.received_requests().await.unwrap_or_default() {
            if req.method == wiremock::http::Method::PATCH
                && req.url.path() == expected_path
                && let Ok(body) = serde_json::from_slice::<serde_json::Value>(&req.body)
            {
                bodies.push(body);
            }
        }
        bodies
    }

    async fn mount_servarrapp_finalizer_patch_mock(mock_server: &MockServer, name: &str) {
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/{name}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrApp",
                "metadata": {
                    "name": name,
                    "namespace": "test",
                    "uid": "test-uid-12345",
                    "resourceVersion": "2"
                },
                "spec": { "app": "Sonarr" }
            })))
            .mount(mock_server)
            .await;
    }

    /// A transient cleanup failure (a 500 from the ServarrApps list call, hit by both the
    /// Prowlarr and Seerr cleanup paths) must keep both finalizers and return an error, so
    /// `error_policy` requeues instead of the app being silently unstuck with the registrations
    /// never actually cleaned up.
    #[tokio::test]
    async fn reconcile_keeps_finalizers_and_errors_on_transient_cleanup_failure() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        mount_kube_status_error(
            &mock_server,
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
            500,
            "unused-seed",
        )
        .await;
        mount_servarrapp_finalizer_patch_mock(&mock_server, "my-sonarr").await;

        let app = make_deleting_app_with_finalizers("my-sonarr", "test");
        let ctx = Arc::new(Context::new(client.clone()));

        let result = reconcile(Arc::new(app), ctx).await;

        assert!(
            matches!(result, Err(Error::CleanupPending)),
            "a transient cleanup failure must surface Error::CleanupPending so error_policy \
             requeues, got: {result:?}"
        );

        // Both cleanups failed, so the computed finalizer list is identical to the existing one
        // — no patch is issued at all (a no-op PATCH would just churn the API server every
        // requeue for a stuck app). The finalizers are retained simply because nothing removed
        // them from the object in the first place.
        let patches = servarrapp_patch_bodies(&mock_server, "my-sonarr").await;
        assert!(
            patches.is_empty(),
            "an unchanged finalizer list must not trigger a no-op patch: {patches:?}"
        );
    }

    /// When only one cleanup target is provably gone, only that finalizer must be dropped — the
    /// other, still-pending finalizer must survive and `reconcile()` must still return
    /// `Error::CleanupPending` for it.
    #[tokio::test]
    async fn reconcile_drops_only_the_finalizer_whose_cleanup_succeeded() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        // Both cleanup bodies share one ServarrApps LIST call. Only an Seerr instance is
        // present, so the Prowlarr cleanup finds nothing to do and succeeds trivially, while the
        // Seerr cleanup finds its target and then fails transiently reading its secret
        // (403, not 404 — a real, retryable failure, not proof the registration is gone).
        mount_servarrapps_list(
            &mock_server,
            json!([seerr_app_json("my-seerr", Some("seerr-secret"))]),
        )
        .await;
        mount_kube_status_error(
            &mock_server,
            "/api/v1/namespaces/test/secrets/seerr-secret",
            403,
            "unused-seed",
        )
        .await;
        mount_servarrapp_finalizer_patch_mock(&mock_server, "my-sonarr").await;

        let app = make_deleting_app_with_finalizers("my-sonarr", "test");
        let ctx = Arc::new(Context::new(client.clone()));
        let result = reconcile(Arc::new(app), ctx).await;

        assert!(
            matches!(result, Err(Error::CleanupPending)),
            "the Seerr finalizer is still pending, so error_policy must requeue: {result:?}"
        );

        let patches = servarrapp_patch_bodies(&mock_server, "my-sonarr").await;
        assert_eq!(patches.len(), 1, "exactly one finalizer patch: {patches:?}");
        let finalizers = patches[0]["metadata"]["finalizers"]
            .as_array()
            .expect("finalizers is an array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            finalizers,
            vec![SEERR_FINALIZER],
            "the succeeded Prowlarr finalizer must be dropped and the still-pending Seerr \
             finalizer must survive: {finalizers:?}"
        );
    }

    /// When the cleanup target is provably gone (secret deleted out-of-band, 404), both
    /// finalizers must be dropped and `reconcile()` must succeed — no indefinite retry over a
    /// target that can never come back.
    #[tokio::test]
    async fn reconcile_removes_finalizers_and_succeeds_when_cleanup_target_already_gone() {
        let mock_server = MockServer::start().await;
        let client = build_mock_client(&mock_server.uri()).await;
        mount_event_post_mock(&mock_server).await;
        mount_servarrapps_list(
            &mock_server,
            json!([
                prowlarr_app_json("my-prowlarr", Some("shared-secret")),
                seerr_app_json("my-seerr", Some("shared-secret")),
            ]),
        )
        .await;
        mount_kube_status_error(
            &mock_server,
            "/api/v1/namespaces/test/secrets/shared-secret",
            404,
            "unused-seed",
        )
        .await;
        mount_servarrapp_finalizer_patch_mock(&mock_server, "my-sonarr").await;

        let app = make_deleting_app_with_finalizers("my-sonarr", "test");
        let ctx = Arc::new(Context::new(client.clone()));

        let result = reconcile(Arc::new(app), ctx).await;

        assert!(
            result.is_ok(),
            "both cleanup targets already gone must be treated as complete: {result:?}"
        );
        assert!(
            event_post_bodies(&mock_server).await.is_empty(),
            "terminal cleanup must not publish any CleanupFailed Event"
        );

        let patches = servarrapp_patch_bodies(&mock_server, "my-sonarr").await;
        assert_eq!(patches.len(), 1, "exactly one finalizer patch: {patches:?}");
        let finalizers = patches[0]["metadata"]["finalizers"]
            .as_array()
            .expect("finalizers is an array");
        assert!(
            finalizers.is_empty(),
            "both finalizers must be dropped once their targets are provably gone: {finalizers:?}"
        );
    }
}
