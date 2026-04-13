use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, PersistentVolumeClaim, Secret, Service};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event, EventType, Recorder};
use kube::runtime::reflector::{self, ObjectRef};
use kube::runtime::watcher;
use kube::{Client, CustomResourceExt, Resource, ResourceExt};
use servarr_api::AppKind;
use servarr_crds::{AppType, Condition, ServarrApp, ServarrAppStatus, condition_types};
use thiserror::Error;
use tokio::time::Duration;
use tracing::{error, info, warn};

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

#[derive(Debug, Error)]
pub enum Error {
    #[error("Kubernetes API error: {0}")]
    Kube(#[source] kube::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
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
    const PROWLARR_FINALIZER: &str = "servarr.dev/prowlarr-sync";
    const OVERSEERR_FINALIZER: &str = "servarr.dev/overseerr-sync";
    if matches!(
        app.spec.app,
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr
    ) {
        if app.metadata.deletion_timestamp.is_some() {
            // App is being deleted — clean up Prowlarr registration
            if let Err(e) =
                cleanup_prowlarr_registration(client, &app, &ns, &recorder, &obj_ref).await
            {
                warn!(%name, error = %e, "failed to clean up Prowlarr registration");
            }
            // App is being deleted — clean up Overseerr registration
            if let Err(e) =
                cleanup_overseerr_registration(client, &app, &ns, &recorder, &obj_ref).await
            {
                warn!(%name, error = %e, "failed to clean up Overseerr registration");
            }
            // Remove finalizers
            let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
            let finalizers: Vec<String> = app
                .metadata
                .finalizers
                .as_ref()
                .map(|f| {
                    f.iter()
                        .filter(|x| *x != PROWLARR_FINALIZER && *x != OVERSEERR_FINALIZER)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let patch = serde_json::json!({
                "metadata": { "finalizers": finalizers }
            });
            sa_api
                .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                .await
                .map_err(Error::Kube)?;
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

        // Ensure Overseerr finalizer is present if an Overseerr with sync enabled exists
        let has_overseerr_finalizer = app
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|f| f.contains(&OVERSEERR_FINALIZER.to_string()));
        if !has_overseerr_finalizer && overseerr_sync_exists(client, &ns).await {
            let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
            let mut finalizers = app.metadata.finalizers.clone().unwrap_or_default();
            finalizers.push(OVERSEERR_FINALIZER.to_string());
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
    if let Some(restore_id) = app
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("servarr.dev/restore-from"))
        .cloned()
    {
        maybe_restore_backup(client, &app, &ns, &name, &restore_id, &recorder, &obj_ref).await;
    }

    // Build and apply Deployment
    let deployment = servarr_resources::deployment::build(&app, &ctx.image_overrides);
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), &ns);
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

    // Build and apply Service
    let service = servarr_resources::service::build(&app);
    let svc_api = Api::<Service>::namespaced(client.clone(), &ns);
    tracing::debug!(%name, "SSA: applying Service");
    svc_api
        .patch(&name, &pp, &Patch::Apply(&service))
        .await
        .map_err(Error::Kube)?;

    // Build and apply PVCs (get-or-create to avoid mutating immutable fields)
    let pvcs = servarr_resources::pvc::build_all(&app);
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
        let np = servarr_resources::networkpolicy::build(&app);
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
    ensure_api_key_secret(client, &app, &ns).await?;

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
        patch_admin_credentials_checksum(client, &app, &ns, &ac.secret_name).await?;
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
    if let Some(route) = servarr_resources::tcproute::build(&app) {
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
    } else if let Some(route) = servarr_resources::httproute::build(&app) {
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
    if let Some(cert) = servarr_resources::certificate::build(&app) {
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

    // API health check and update check (non-blocking)
    let (health_condition, update_condition) = check_api_health(client, &app, &ns).await;

    // Admin credential sync via live API (SABnzbd, Transmission, Jellyfin, Tautulli, Overseerr)
    let admin_creds_condition = sync_admin_credentials(client, &app, &ns).await;
    // If sync failed (app not ready yet), requeue sooner than the default 300s so
    // credentials are applied once the app becomes healthy.
    let admin_creds_pending = admin_creds_condition
        .as_ref()
        .map(|c| c.status != "True")
        .unwrap_or(false);

    // Backup scheduling (non-blocking)
    let backup_status = maybe_run_backup(client, &app, &ns, &recorder, &obj_ref).await;

    // Prowlarr cross-app sync (only for Prowlarr-type apps with sync enabled)
    if app.spec.app == AppType::Prowlarr
        && let Some(ref sync_spec) = app.spec.prowlarr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        if let Err(e) = sync_prowlarr_apps(client, &app, target_ns, &recorder, &obj_ref).await {
            warn!(%name, error = %e, "Prowlarr sync failed");
        }
    }

    // Overseerr cross-app sync (only for Overseerr-type apps with sync enabled)
    if app.spec.app == AppType::Overseerr
        && let Some(ref sync_spec) = app.spec.overseerr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        if let Err(e) = sync_overseerr_servers(client, &app, target_ns, &recorder, &obj_ref).await {
            warn!(%name, error = %e, "Overseerr sync failed");
        }
    }

    // Bazarr cross-app sync
    if app.spec.app == AppType::Bazarr
        && let Some(ref sync_spec) = app.spec.bazarr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        if let Err(e) = sync_bazarr_apps(client, &app, target_ns).await {
            warn!(%name, error = %e, "Bazarr sync failed");
        }
    }

    // Subgen → Jellyfin sync
    if app.spec.app == AppType::Subgen
        && let Some(ref sync_spec) = app.spec.subgen_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        if let Err(e) = sync_subgen_jellyfin(client, &app, target_ns).await {
            warn!(%name, error = %e, "Subgen Jellyfin sync failed");
        }
    }

    // Update status
    tracing::debug!(%name, "updating status");
    update_status(
        client,
        &app,
        &ns,
        &name,
        StatusConditions {
            health: health_condition,
            update: update_condition,
            admin_creds: admin_creds_condition,
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
async fn ensure_api_key_secret(client: &Client, app: &ServarrApp, ns: &str) -> Result<(), Error> {
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

    use rand::Rng as _;
    let key: String = rand::rng()
        .sample_iter(rand::distr::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

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
    ns: &str,
    secret_name: &str,
) -> Result<(), Error> {
    use sha2::{Digest, Sha256};

    let username = match servarr_api::read_secret_key(client, ns, secret_name, "username").await {
        Ok(v) => v,
        Err(e) => {
            warn!(
                app = %app.name_any(),
                secret = %secret_name,
                error = %e,
                "admin-credentials: failed to read secret for checksum"
            );
            return Ok(());
        }
    };
    let password = match servarr_api::read_secret_key(client, ns, secret_name, "password").await {
        Ok(v) => v,
        Err(e) => {
            warn!(
                app = %app.name_any(),
                secret = %secret_name,
                error = %e,
                "admin-credentials: failed to read secret for checksum"
            );
            return Ok(());
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(username.as_bytes());
    hasher.update(b":");
    hasher.update(password.as_bytes());
    let checksum = format!("{:x}", hasher.finalize());

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
async fn sync_admin_credentials(client: &Client, app: &ServarrApp, ns: &str) -> Option<Condition> {
    let ac = app.spec.admin_credentials.as_ref()?;
    let now = chrono_now();

    let username = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "username").await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(app = %app.name_any(), error = %e, "admin-credentials: failed to read username");
            return Some(Condition {
                condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED.to_string(),
                status: "Unknown".to_string(),
                reason: "SecretReadError".to_string(),
                message: e.to_string(),
                last_transition_time: now,
            });
        }
    };
    let password = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "password").await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(app = %app.name_any(), error = %e, "admin-credentials: failed to read password");
            return Some(Condition {
                condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED.to_string(),
                status: "Unknown".to_string(),
                reason: "SecretReadError".to_string(),
                message: e.to_string(),
                last_transition_time: now,
            });
        }
    };

    let app_name = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
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
                            message: e.to_string(),
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
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        AppType::Transmission => {
            // Try to enable auth without credentials first (Transmission starts with auth
            // disabled when LSIO's env var mechanism doesn't fire).  If we get 401,
            // auth is already enabled (e.g., by LSIO or a previous reconcile) and our
            // credentials should already be correct; confirm by fetching session info.
            info!(app = %app.name_any(), url = %base_url, "admin-credentials: syncing Transmission RPC auth");
            match servarr_api::TransmissionClient::new(&base_url, None, None) {
                Ok(c_no_auth) => match c_no_auth.session_set_auth(&username, &password).await {
                    Ok(()) => {
                        info!(app = %app.name_any(), "admin-credentials: Transmission session-set succeeded (auth now enabled)");
                        Ok(())
                    }
                    Err(servarr_api::ApiError::ApiResponse { status: 401, .. }) => {
                        info!(app = %app.name_any(), "admin-credentials: Transmission auth already enabled, verifying credentials");
                        match servarr_api::TransmissionClient::new(
                            &base_url,
                            Some(&username),
                            Some(&password),
                        ) {
                            Ok(c_auth) => c_auth
                                .session_get()
                                .await
                                .map(|_| ())
                                .map_err(|e| e.to_string()),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    Err(e) => {
                        warn!(app = %app.name_any(), error = %e, "admin-credentials: Transmission session-set failed");
                        Err(e.to_string())
                    }
                },
                Err(e) => Err(e.to_string()),
            }
        }
        AppType::Jellyfin => match servarr_api::JellyfinClient::new(&base_url) {
            Ok(c) => c
                .configure_admin(&username, &password)
                .await
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        },
        AppType::Tautulli => match servarr_api::TautulliClient::new(&base_url) {
            Ok(c) => c
                .set_credentials(&username, &password)
                .await
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        },
        AppType::Overseerr => {
            let api_key = match app.spec.api_key_secret.as_deref() {
                Some(s) => match servarr_api::read_secret_key(client, ns, s, "api-key").await {
                    Ok(k) => k,
                    Err(e) => {
                        return Some(Condition {
                            condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED
                                .to_string(),
                            status: "Unknown".to_string(),
                            reason: "ApiKeyReadError".to_string(),
                            message: e.to_string(),
                            last_transition_time: now,
                        });
                    }
                },
                None => {
                    return Some(Condition::fail(
                        condition_types::ADMIN_CREDENTIALS_CONFIGURED,
                        "NoApiKey",
                        "Overseerr credential sync requires apiKeySecret to be set",
                        &now,
                    ));
                }
            };
            let c = servarr_api::OverseerrClient::new(&base_url, &api_key);
            c.setup_local_auth(&username, &password)
                .await
                .map_err(|e| e.to_string())
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
                            message: e.to_string(),
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
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
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
                        message: e.to_string(),
                        last_transition_time: now,
                    });
                }
            };
            match servarr_api::BazarrClient::new(&base_url, &api_key) {
                Ok(c) => {
                    let password_md5 = format!("{:x}", md5::compute(password.as_bytes()));
                    c.set_credentials(&username, &password_md5)
                        .await
                        .map_err(|e| e.to_string())
                }
                Err(e) => Err(e.to_string()),
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

pub(crate) async fn check_api_health(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
) -> (Option<Condition>, Option<Condition>) {
    let _health_check = match app.spec.api_health_check.as_ref() {
        Some(hc) if hc.enabled => hc,
        _ => return (None, None),
    };
    let secret_name = match app.spec.api_key_secret.as_deref() {
        Some(s) => s,
        None => return (None, None),
    };

    let now = chrono_now();
    let api_key = match servarr_api::read_secret_key(client, ns, secret_name, "api-key").await {
        Ok(k) => k,
        Err(e) => {
            warn!(error = %e, "failed to read API key secret");
            let cond = Condition {
                condition_type: condition_types::APP_HEALTHY.to_string(),
                status: "Unknown".to_string(),
                reason: "SecretReadError".to_string(),
                message: e.to_string(),
                last_transition_time: now,
            };
            return (Some(cond), None);
        }
    };

    let app_name = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    use servarr_api::HealthCheck;
    let (healthy, update_cond): (Result<bool, String>, Option<Condition>) = match app.spec.app {
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr => {
            let Some(app_kind) = app_type_to_kind(&app.spec.app) else {
                return (None, None);
            };
            match servarr_api::ServarrClient::new(&base_url, &api_key, app_kind) {
                Ok(c) => {
                    let h = c.is_healthy().await.map_err(|e| e.to_string());
                    let uc = check_update_available(&c, &now).await;
                    (h, uc)
                }
                Err(e) => (Err(e.to_string()), None),
            }
        }
        AppType::Sabnzbd => match servarr_api::SabnzbdClient::new(&base_url, &api_key) {
            Ok(c) => {
                let h = c.is_healthy().await.map_err(|e| e.to_string());
                (h, None)
            }
            Err(e) => (Err(e.to_string()), None),
        },
        AppType::Transmission => {
            // Pass credentials to the health check client when adminCredentials is set.
            let (tx_user, tx_pass): (Option<String>, Option<String>) = if let Some(ref ac) =
                app.spec.admin_credentials
            {
                let u = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "username")
                    .await
                {
                    Ok(v) => Some(v),
                    Err(e) => {
                        warn!(app = %app.name_any(), error = %e,
                                "health-check: failed to read Transmission username, proceeding unauthenticated");
                        None
                    }
                };
                let p = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "password")
                    .await
                {
                    Ok(v) => Some(v),
                    Err(e) => {
                        warn!(app = %app.name_any(), error = %e,
                                "health-check: failed to read Transmission password, proceeding unauthenticated");
                        None
                    }
                };
                (u, p)
            } else {
                (None, None)
            };
            match servarr_api::TransmissionClient::new(
                &base_url,
                tx_user.as_deref(),
                tx_pass.as_deref(),
            ) {
                Ok(c) => {
                    let h = c.is_healthy().await.map_err(|e| e.to_string());
                    (h, None)
                }
                Err(e) => (Err(e.to_string()), None),
            }
        }
        AppType::Jellyfin => match servarr_api::JellyfinClient::new(&base_url) {
            Ok(c) => {
                let h = c.is_healthy().await.map_err(|e| e.to_string());
                (h, None)
            }
            Err(e) => (Err(e.to_string()), None),
        },
        AppType::Plex => match servarr_api::PlexClient::new(&base_url) {
            Ok(c) => {
                let h = c.is_healthy().await.map_err(|e| e.to_string());
                (h, None)
            }
            Err(e) => (Err(e.to_string()), None),
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

async fn check_update_available(
    client: &servarr_api::ServarrClient,
    now: &str,
) -> Option<Condition> {
    let updates = match client.updates().await {
        Ok(u) => u,
        Err(e) => {
            tracing::debug!(error = %e, "failed to fetch updates, skipping update condition");
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

pub(crate) struct StatusConditions {
    pub health: Option<Condition>,
    pub update: Option<Condition>,
    pub admin_creds: Option<Condition>,
}

pub(crate) async fn update_status(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    name: &str,
    conditions: StatusConditions,
    backup_status: Option<servarr_crds::BackupStatus>,
) -> Result<(), Error> {
    let StatusConditions {
        health: health_condition,
        update: update_condition,
        admin_creds: admin_creds_condition,
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
            warn!(%name, error = %e, "failed to get Deployment for status check, reporting not-ready");
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

    // API health condition
    if let Some(cond) = health_condition {
        status.set_condition(cond);
    }
    // Update available condition
    if let Some(cond) = update_condition {
        status.set_condition(cond);
    }
    // Admin credentials condition
    if let Some(cond) = admin_creds_condition {
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
    warn!(%error, "reconciliation failed, requeuing");

    let recorder = Recorder::new(ctx.client.clone(), ctx.reporter.clone());
    let obj_ref = app.object_ref(&());
    let error_msg = error.to_string();
    tokio::spawn(async move {
        let _ = recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "ReconcileError".into(),
                    note: Some(error_msg),
                    action: "Reconcile".into(),
                    secondary: None,
                },
                &obj_ref,
            )
            .await;
    });

    Action::requeue(Duration::from_secs(60))
}

async fn maybe_run_backup(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Option<servarr_crds::BackupStatus> {
    let backup_spec = app.spec.backup.as_ref()?;
    if !backup_spec.enabled || backup_spec.schedule.is_empty() {
        return None;
    }

    let secret_name = app.spec.api_key_secret.as_deref()?;
    let api_key = match servarr_api::read_secret_key(client, ns, secret_name, "api-key").await {
        Ok(k) => k,
        Err(e) => {
            warn!(error = %e, "backup: failed to read API key");
            return Some(servarr_crds::BackupStatus {
                last_backup_result: Some(format!("secret read error: {e}")),
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

    // Check if backup is due based on cron schedule
    let schedule = match cron::Schedule::from_str(&backup_spec.schedule) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, schedule = %backup_spec.schedule, "invalid cron schedule");
            return Some(servarr_crds::BackupStatus {
                last_backup_result: Some(format!("invalid schedule: {e}")),
                ..Default::default()
            });
        }
    };

    use chrono::Utc;
    let now = Utc::now();

    // Check last backup time from existing status
    let last_backup = app
        .status
        .as_ref()
        .and_then(|s| s.backup_status.as_ref())
        .and_then(|bs| bs.last_backup_time.as_deref())
        .and_then(|t| t.parse::<chrono::DateTime<Utc>>().ok());

    let is_due = match last_backup {
        Some(last) => schedule.after(&last).take(1).any(|next| next <= now),
        None => true, // Never backed up, do it now
    };

    if !is_due {
        // Return existing status unchanged
        return app.status.as_ref().and_then(|s| s.backup_status.clone());
    }

    let app_name = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    let app_kind = app_type_to_kind(&app.spec.app)?;
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
                        warn!(backup_id = old.id, error = %e, "failed to prune old backup");
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
            warn!(app = %app_name, error = %e, "backup failed");
            increment_backup_operations(app_type, "backup", "error");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "BackupFailed".into(),
                        note: Some(format!("Backup failed: {e}")),
                        action: "Backup".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;
            Some(servarr_crds::BackupStatus {
                last_backup_time: last_backup.map(|_| chrono_now()),
                last_backup_result: Some(format!("error: {e}")),
                backup_count: 0,
            })
        }
    }
}

/// Handle restore-from-backup triggered by the `servarr.dev/restore-from` annotation.
/// Scales the Deployment to 0, calls restore via the API, scales back up, and removes
/// the annotation to prevent re-triggering.
async fn maybe_restore_backup(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    name: &str,
    restore_id: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) {
    // Only Servarr v3 apps support backup/restore API
    if !matches!(
        app.spec.app,
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr
    ) {
        warn!(%name, app_type = ?app.spec.app, "restore-from annotation set on unsupported app type, ignoring");
        return;
    }

    let backup_id: i64 = match restore_id.parse() {
        Ok(id) => id,
        Err(_) => {
            warn!(%name, restore_id, "invalid restore-from annotation value, expected integer backup ID");
            return;
        }
    };

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

    let scale_down = serde_json::json!({
        "spec": { "replicas": 0 }
    });
    if let Err(e) = deploy_api
        .patch(name, &PatchParams::default(), &Patch::Merge(scale_down))
        .await
    {
        warn!(%name, error = %e, "failed to scale down for restore");
        return;
    }

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
                warn!(%name, error = %e, "failed to check deployment status during restore");
                break;
            }
        }
    }

    // Step 2: Build API client and call restore
    let api_key = match app.spec.api_key_secret.as_deref() {
        Some(secret_name) => {
            match servarr_api::read_secret_key(client, ns, secret_name, "api-key").await {
                Ok(k) => k,
                Err(e) => {
                    warn!(%name, error = %e, "failed to read API key for restore");
                    let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
                    if let Err(se) = deploy_api
                        .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
                        .await
                    {
                        warn!(%name, error = %se, "failed to scale back up after restore error; deployment may be at zero replicas");
                    }
                    return;
                }
            }
        }
        None => {
            warn!(%name, "no api_key_secret configured, cannot restore");
            let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
            if let Err(e) = deploy_api
                .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
                .await
            {
                warn!(%name, error = %e, "failed to scale back up; deployment may be at zero replicas");
            }
            return;
        }
    };

    let app_name = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    // Only Servarr v3 apps (Sonarr/Radarr/Lidarr/Prowlarr) support backup/restore
    let Some(app_kind) = app_type_to_kind(&app.spec.app) else {
        warn!(%name, app_type = ?app.spec.app, "restore: app type has no AppKind mapping");
        return;
    };
    let restore_result = match servarr_api::ServarrClient::new(&base_url, &api_key, app_kind) {
        Ok(c) => c.restore_backup(backup_id).await,
        Err(e) => {
            warn!(%name, error = %e, "failed to create API client for restore");
            let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
            if let Err(se) = deploy_api
                .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
                .await
            {
                warn!(%name, error = %se, "failed to scale back up after client error; deployment may be at zero replicas");
            }
            return;
        }
    };

    match restore_result {
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
        }
        Err(e) => {
            warn!(%name, backup_id, error = %e, "restore API call failed");
            increment_backup_operations(app.spec.app.as_str(), "restore", "error");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "RestoreFailed".into(),
                        note: Some(format!("Failed to restore from backup {backup_id}: {e}")),
                        action: "Restore".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;
        }
    }

    // Step 3: Scale back up
    let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
    if let Err(e) = deploy_api
        .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
        .await
    {
        warn!(%name, error = %e, "failed to scale back up after restore");
    }

    // Step 4: Remove the restore-from annotation to prevent re-triggering
    let servarr_api_resource = Api::<ServarrApp>::namespaced(client.clone(), ns);
    let remove_annotation = serde_json::json!({
        "metadata": {
            "annotations": {
                "servarr.dev/restore-from": null
            }
        }
    });
    if let Err(e) = servarr_api_resource
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(remove_annotation),
        )
        .await
    {
        warn!(%name, error = %e, "failed to remove restore-from annotation");
    }
}

/// A discovered *arr app in the namespace with its service URL and API key.
#[derive(Debug)]
pub(crate) struct DiscoveredApp {
    pub(crate) name: String,
    pub(crate) app_type: AppType,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) instance: Option<String>,
}

/// Discover all Servarr v3 apps (Sonarr/Radarr/Lidarr) in a namespace
/// and resolve their service URLs and API keys.
pub(crate) async fn discover_namespace_apps(
    client: &Client,
    namespace: &str,
) -> Result<Vec<DiscoveredApp>, anyhow::Error> {
    use kube::api::ListParams;

    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = api
        .list(&ListParams::default())
        .await
        .map_err(|e| anyhow::anyhow!("failed to list ServarrApps: {e}"))?;

    let mut discovered = Vec::new();
    for app in &apps {
        // Only sync Servarr v3 apps (they share the /api/v3 interface)
        if !matches!(
            app.spec.app,
            AppType::Sonarr | AppType::Radarr | AppType::Lidarr
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
                warn!(app = %app.name_any(), error = %e, "skipping app: failed to read API key");
                continue;
            }
        };

        let app_name = servarr_resources::common::app_name(app);
        let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
        let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
        let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
        let base_url = format!("http://{app_name}.{namespace}.svc:{port}");

        discovered.push(DiscoveredApp {
            name: app.name_any(),
            app_type: app.spec.app.clone(),
            base_url,
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
) -> Result<(), anyhow::Error> {
    let prowlarr_name = prowlarr.name_any();
    let ns = prowlarr.namespace().unwrap_or_else(|| "default".into());

    // Build Prowlarr client
    let secret_name = prowlarr
        .spec
        .api_key_secret
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Prowlarr sync requires api_key_secret"))?;
    let prowlarr_key = servarr_api::read_secret_key(client, &ns, secret_name, "api-key").await?;

    let prowlarr_app_name = servarr_resources::common::app_name(prowlarr);
    let defaults = servarr_crds::AppDefaults::for_app(&prowlarr.spec.app);
    let svc_spec = prowlarr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let prowlarr_url = format!("http://{prowlarr_app_name}.{ns}.svc:{port}");

    let prowlarr_client = servarr_api::ProwlarrClient::new(&prowlarr_url, &prowlarr_key)?;

    // Discover apps in target namespace
    let discovered = discover_namespace_apps(client, target_ns).await?;

    // Get current Prowlarr applications
    let existing = prowlarr_client.list_applications().await?;

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
        synced_urls.insert(app.base_url.clone());

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
                    value: serde_json::Value::String(app.base_url.clone()),
                },
                servarr_api::prowlarr::ProwlarrAppField {
                    name: "apiKey".into(),
                    value: serde_json::Value::String(app.api_key.clone()),
                },
            ],
            tags: Vec::new(),
        };

        if let Some(existing_app) = existing_by_url.get(&app.base_url) {
            // Update if name changed
            if existing_app.name != app.name {
                info!(prowlarr = %prowlarr_name, app = %app.name, "updating Prowlarr application");
                let mut updated = new_app;
                updated.id = existing_app.id;
                if let Err(e) = prowlarr_client
                    .update_application(existing_app.id, &updated)
                    .await
                {
                    warn!(app = %app.name, error = %e, "failed to update Prowlarr application");
                }
            }
        } else {
            // Add new
            info!(prowlarr = %prowlarr_name, app = %app.name, "adding application to Prowlarr");
            if let Err(e) = prowlarr_client.add_application(&new_app).await {
                warn!(app = %app.name, error = %e, "failed to add Prowlarr application");
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
                    warn!(app = %app.name, error = %e, "failed to remove Prowlarr application");
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
            warn!(error = %e, %namespace, "failed to list ServarrApps for prowlarr-sync check, assuming no sync exists");
            false
        }
    }
}

/// Remove this app's registration from Prowlarr when the CR is deleted.
async fn cleanup_prowlarr_registration(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    use kube::api::ListParams;

    let app_name_str = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let app_url = format!("http://{app_name_str}.{namespace}.svc:{port}");

    // Find the Prowlarr instance
    let sa_api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = sa_api.list(&ListParams::default()).await?;
    let prowlarr = apps.iter().find(|a| {
        a.spec.app == AppType::Prowlarr && a.spec.prowlarr_sync.as_ref().is_some_and(|s| s.enabled)
    });

    let prowlarr = match prowlarr {
        Some(p) => p,
        None => return Ok(()), // No Prowlarr with sync, nothing to clean up
    };

    let secret_name = match prowlarr.spec.api_key_secret.as_deref() {
        Some(s) => s,
        None => return Ok(()),
    };

    let prowlarr_key =
        servarr_api::read_secret_key(client, namespace, secret_name, "api-key").await?;

    let prowlarr_app_name = servarr_resources::common::app_name(prowlarr);
    let prowlarr_defaults = servarr_crds::AppDefaults::for_app(&prowlarr.spec.app);
    let prowlarr_svc = prowlarr
        .spec
        .service
        .as_ref()
        .unwrap_or(&prowlarr_defaults.service);
    let prowlarr_port = prowlarr_svc.ports.first().map(|p| p.port).unwrap_or(80);
    let prowlarr_ns = prowlarr.namespace().unwrap_or_else(|| namespace.into());
    let prowlarr_url = format!("http://{prowlarr_app_name}.{prowlarr_ns}.svc:{prowlarr_port}");

    let prowlarr_client = servarr_api::ProwlarrClient::new(&prowlarr_url, &prowlarr_key)?;

    let existing = prowlarr_client.list_applications().await?;
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
        prowlarr_client.delete_application(registered.id).await?;

        let _ = recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "ProwlarrCleanup".into(),
                    note: Some(format!("Removed {} from Prowlarr", app.name_any())),
                    action: "Finalize".into(),
                    secondary: None,
                },
                obj_ref,
            )
            .await;
    }

    Ok(())
}

/// Sync discovered Sonarr/Radarr apps into Overseerr as registered servers.
async fn sync_overseerr_servers(
    client: &Client,
    overseerr: &ServarrApp,
    target_ns: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    let overseerr_name = overseerr.name_any();
    let ns = overseerr.namespace().unwrap_or_else(|| "default".into());

    // Build Overseerr client
    let secret_name = overseerr
        .spec
        .api_key_secret
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Overseerr sync requires api_key_secret"))?;
    let overseerr_key = servarr_api::read_secret_key(client, &ns, secret_name, "api-key").await?;

    let overseerr_app_name = servarr_resources::common::app_name(overseerr);
    let defaults = servarr_crds::AppDefaults::for_app(&overseerr.spec.app);
    let svc_spec = overseerr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let overseerr_url = format!("http://{overseerr_app_name}.{ns}.svc:{port}");

    let overseerr_client = servarr_api::OverseerrClient::new(&overseerr_url, &overseerr_key);

    // Discover Sonarr/Radarr apps in target namespace
    let discovered = discover_namespace_apps(client, target_ns).await?;

    // Get existing server registrations
    let existing_sonarr = overseerr_client.list_sonarr().await?;
    let existing_radarr = overseerr_client.list_radarr().await?;

    // Get Overseerr config for default profile/directory settings
    let overseerr_config = match &overseerr.spec.app_config {
        Some(servarr_crds::AppConfig::Overseerr(c)) => Some(c.as_ref()),
        _ => None,
    };

    let auto_remove = overseerr
        .spec
        .overseerr_sync
        .as_ref()
        .map(|s| s.auto_remove)
        .unwrap_or(true);

    // Track which hostname:port combos we sync so we can detect stale entries
    let mut synced_sonarr_keys = std::collections::HashSet::new();
    let mut synced_radarr_keys = std::collections::HashSet::new();

    for app in &discovered {
        let url = url::Url::parse(&app.base_url)
            .map_err(|e| anyhow::anyhow!("invalid base_url for {}: {e}", app.base_url))?;
        let hostname = url.host_str().unwrap_or("").to_string();
        let port = url.port().unwrap_or(80) as f64;
        let is4k = app.instance.as_deref() == Some("4k");

        match app.app_type {
            AppType::Sonarr => {
                let key = (hostname.clone(), port as u16);
                synced_sonarr_keys.insert(key);

                let sonarr_defaults = overseerr_config.and_then(|c| c.sonarr.as_ref());
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
                    if let Err(e) = overseerr_client.update_sonarr(id, updated).await {
                        warn!(app = %app.name, error = %e, "failed to update Sonarr in Overseerr");
                    }
                } else {
                    info!(overseerr = %overseerr_name, app = %app.name, "adding Sonarr server to Overseerr");
                    if let Err(e) = overseerr_client.create_sonarr(settings).await {
                        warn!(app = %app.name, error = %e, "failed to add Sonarr to Overseerr");
                    }
                }
            }
            AppType::Radarr => {
                let key = (hostname.clone(), port as u16);
                synced_radarr_keys.insert(key);

                let radarr_defaults = overseerr_config.and_then(|c| c.radarr.as_ref());
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
                    if let Err(e) = overseerr_client.update_radarr(id, updated).await {
                        warn!(app = %app.name, error = %e, "failed to update Radarr in Overseerr");
                    }
                } else {
                    info!(overseerr = %overseerr_name, app = %app.name, "adding Radarr server to Overseerr");
                    if let Err(e) = overseerr_client.create_radarr(settings).await {
                        warn!(app = %app.name, error = %e, "failed to add Radarr to Overseerr");
                    }
                }
            }
            _ => continue,
        }
    }

    // Remove stale servers
    if auto_remove {
        for existing in &existing_sonarr {
            let key = (existing.hostname.clone(), existing.port as u16);
            if !synced_sonarr_keys.contains(&key) {
                let id = existing.id.unwrap_or(0.0) as i32;
                info!(overseerr = %overseerr_name, server = %existing.name, "removing stale Sonarr server from Overseerr");
                if let Err(e) = overseerr_client.delete_sonarr(id).await {
                    warn!(server = %existing.name, error = %e, "failed to remove stale Sonarr from Overseerr");
                }
            }
        }
        for existing in &existing_radarr {
            let key = (existing.hostname.clone(), existing.port as u16);
            if !synced_radarr_keys.contains(&key) {
                let id = existing.id.unwrap_or(0.0) as i32;
                info!(overseerr = %overseerr_name, server = %existing.name, "removing stale Radarr server from Overseerr");
                if let Err(e) = overseerr_client.delete_radarr(id).await {
                    warn!(server = %existing.name, error = %e, "failed to remove stale Radarr from Overseerr");
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
                reason: "OverseerrSyncComplete".into(),
                note: Some(format!(
                    "Synced {sonarr_count} Sonarr + {radarr_count} Radarr servers to Overseerr"
                )),
                action: "OverseerrSync".into(),
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
) -> Result<(), anyhow::Error> {
    let bazarr_name = bazarr.name_any();
    let ns = bazarr.namespace().unwrap_or_else(|| "default".into());

    // Read Bazarr's operator-managed API key
    let api_key_secret = servarr_resources::common::child_name(bazarr, "api-key");
    let bazarr_key = servarr_api::read_secret_key(client, &ns, &api_key_secret, "api-key").await?;

    let bazarr_app_name = servarr_resources::common::app_name(bazarr);
    let defaults = servarr_crds::AppDefaults::for_app(&bazarr.spec.app);
    let svc_spec = bazarr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let bazarr_url = format!("http://{bazarr_app_name}.{ns}.svc:{port}");

    let bazarr_client = servarr_api::BazarrClient::new(&bazarr_url, &bazarr_key)?;

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

    for app in &discovered {
        let url = url::Url::parse(&app.base_url)
            .map_err(|e| anyhow::anyhow!("invalid companion URL {}: {e}", app.base_url))?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("no host in {}", app.base_url))?
            .to_string();
        let companion_port = url.port().unwrap_or(80);

        match app.app_type {
            AppType::Sonarr => {
                info!(bazarr = %bazarr_name, sonarr = %app.name, "syncing Sonarr into Bazarr");
                if let Err(e) = bazarr_client
                    .configure_sonarr(&host, companion_port, &app.api_key)
                    .await
                {
                    warn!(bazarr = %bazarr_name, sonarr = %app.name, error = %e,
                        "failed to configure Sonarr in Bazarr");
                }
            }
            AppType::Radarr => {
                info!(bazarr = %bazarr_name, radarr = %app.name, "syncing Radarr into Bazarr");
                if let Err(e) = bazarr_client
                    .configure_radarr(&host, companion_port, &app.api_key)
                    .await
                {
                    warn!(bazarr = %bazarr_name, radarr = %app.name, error = %e,
                        "failed to configure Radarr in Bazarr");
                }
            }
            _ => {}
        }
    }

    if auto_remove {
        if !has_sonarr && let Err(e) = bazarr_client.disable_sonarr().await {
            warn!(bazarr = %bazarr_name, error = %e, "failed to disable Sonarr in Bazarr");
        }
        if !has_radarr && let Err(e) = bazarr_client.disable_radarr().await {
            warn!(bazarr = %bazarr_name, error = %e, "failed to disable Radarr in Bazarr");
        }
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
) -> Result<(), anyhow::Error> {
    let subgen_name = subgen.name_any();
    let ns = subgen.namespace().unwrap_or_else(|| "default".into());

    // Find Jellyfin in target namespace
    let all_apps = Api::<ServarrApp>::namespaced(client.clone(), target_ns);
    let app_list = all_apps
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| anyhow::anyhow!("failed to list ServarrApps: {e}"))?;

    let jellyfin = match app_list
        .items
        .iter()
        .find(|a| a.spec.app == AppType::Jellyfin)
    {
        Some(j) => j,
        None => {
            warn!(subgen = %subgen_name,
                "subgen-sync: no Jellyfin CR found in namespace {target_ns}, skipping");
            return Ok(());
        }
    };

    // Verify Jellyfin's API key secret is accessible (fail fast before patching Deployment).
    let jf_secret_name = match jellyfin.spec.api_key_secret.as_deref() {
        Some(s) => s.to_string(),
        None => {
            warn!(subgen = %subgen_name,
                "subgen-sync: Jellyfin CR has no apiKeySecret, skipping");
            return Ok(());
        }
    };
    // Verify the secret is readable; the Deployment will reference it via secretKeyRef.
    servarr_api::read_secret_key(client, target_ns, &jf_secret_name, "api-key")
        .await
        .map_err(|e| anyhow::anyhow!("Jellyfin API key secret {jf_secret_name} unreadable: {e}"))?;

    let jf_app_name = servarr_resources::common::app_name(jellyfin);
    let jf_defaults = servarr_crds::AppDefaults::for_app(&jellyfin.spec.app);
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
        .map_err(|e| anyhow::anyhow!("failed to patch Subgen Deployment: {e}"))?;

    info!(subgen = %subgen_name, jellyfin = %jf_app_name, "subgen-sync: injected Jellyfin env vars");
    Ok(())
}

/// Check if any Overseerr instance with overseerr_sync.enabled exists in the namespace.
async fn overseerr_sync_exists(client: &Client, namespace: &str) -> bool {
    use kube::api::ListParams;
    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    match api.list(&ListParams::default()).await {
        Ok(list) => list.iter().any(|a| {
            a.spec.app == AppType::Overseerr
                && a.spec.overseerr_sync.as_ref().is_some_and(|s| s.enabled)
        }),
        Err(e) => {
            warn!(error = %e, %namespace, "failed to list ServarrApps for overseerr-sync check, assuming no sync exists");
            false
        }
    }
}

/// Remove this app's registration from Overseerr when the CR is deleted.
async fn cleanup_overseerr_registration(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    use kube::api::ListParams;

    let app_name_str = servarr_resources::common::app_name(app);
    let defaults_for_app = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app
        .spec
        .service
        .as_ref()
        .unwrap_or(&defaults_for_app.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let app_hostname = format!("{app_name_str}.{namespace}.svc");

    // Find the Overseerr instance
    let sa_api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = sa_api.list(&ListParams::default()).await?;
    let overseerr = apps.iter().find(|a| {
        a.spec.app == AppType::Overseerr
            && a.spec.overseerr_sync.as_ref().is_some_and(|s| s.enabled)
    });

    let overseerr = match overseerr {
        Some(o) => o,
        None => return Ok(()),
    };

    let secret_name = match overseerr.spec.api_key_secret.as_deref() {
        Some(s) => s,
        None => return Ok(()),
    };

    let overseerr_ns = overseerr.namespace().unwrap_or_else(|| namespace.into());
    let overseerr_key =
        servarr_api::read_secret_key(client, &overseerr_ns, secret_name, "api-key").await?;

    let overseerr_app_name = servarr_resources::common::app_name(overseerr);
    let overseerr_defaults = servarr_crds::AppDefaults::for_app(&overseerr.spec.app);
    let overseerr_svc = overseerr
        .spec
        .service
        .as_ref()
        .unwrap_or(&overseerr_defaults.service);
    let overseerr_port = overseerr_svc.ports.first().map(|p| p.port).unwrap_or(80);
    let overseerr_url = format!("http://{overseerr_app_name}.{overseerr_ns}.svc:{overseerr_port}");

    let overseerr_client = servarr_api::OverseerrClient::new(&overseerr_url, &overseerr_key);

    // Remove matching Sonarr or Radarr server by hostname + port
    match app.spec.app {
        AppType::Sonarr => {
            let existing = overseerr_client.list_sonarr().await?;
            if let Some(registered) = existing
                .iter()
                .find(|s| s.hostname == app_hostname && s.port == port as f64)
            {
                let id = registered.id.unwrap_or(0.0) as i32;
                info!(
                    app = %app.name_any(),
                    overseerr_server_id = id,
                    "removing Sonarr from Overseerr on deletion"
                );
                overseerr_client.delete_sonarr(id).await?;

                let _ = recorder
                    .publish(
                        &Event {
                            type_: EventType::Normal,
                            reason: "OverseerrCleanup".into(),
                            note: Some(format!("Removed {} from Overseerr", app.name_any())),
                            action: "Finalize".into(),
                            secondary: None,
                        },
                        obj_ref,
                    )
                    .await;
            }
        }
        AppType::Radarr => {
            let existing = overseerr_client.list_radarr().await?;
            if let Some(registered) = existing
                .iter()
                .find(|s| s.hostname == app_hostname && s.port == port as f64)
            {
                let id = registered.id.unwrap_or(0.0) as i32;
                info!(
                    app = %app.name_any(),
                    overseerr_server_id = id,
                    "removing Radarr from Overseerr on deletion"
                );
                overseerr_client.delete_radarr(id).await?;

                let _ = recorder
                    .publish(
                        &Event {
                            type_: EventType::Normal,
                            reason: "OverseerrCleanup".into(),
                            note: Some(format!("Removed {} from Overseerr", app.name_any())),
                            action: "Finalize".into(),
                            secondary: None,
                        },
                        obj_ref,
                    )
                    .await;
            }
        }
        _ => {}
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

    // ---- print_crd ----

    #[test]
    fn print_crd_returns_ok() {
        assert!(print_crd().is_ok());
    }

    // ---- prowlarr_sync_exists ----

    #[tokio::test]
    async fn prowlarr_sync_exists_returns_true_when_prowlarr_with_sync() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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

    // ---- Helper: build kube::Client from mock server ----

    async fn build_mock_client(server_uri: &str) -> Client {
        use kube::config::{
            AuthInfo, Cluster, Context as KubeContext, KubeConfigOptions, Kubeconfig,
            NamedAuthInfo, NamedCluster, NamedContext,
        };

        let kubeconfig = Kubeconfig {
            clusters: vec![NamedCluster {
                name: "test".into(),
                cluster: Some(Cluster {
                    server: Some(server_uri.to_string()),
                    insecure_skip_tls_verify: Some(true),
                    ..Default::default()
                }),
            }],
            contexts: vec![NamedContext {
                name: "test".into(),
                context: Some(KubeContext {
                    cluster: "test".into(),
                    user: Some("test".into()),
                    namespace: Some("test".into()),
                    ..Default::default()
                }),
            }],
            auth_infos: vec![NamedAuthInfo {
                name: "test".into(),
                auth_info: Some(AuthInfo::default()),
            }],
            current_context: Some("test".into()),
            ..Default::default()
        };

        let config =
            kube::Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
                .await
                .unwrap();
        Client::try_from(config).unwrap()
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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
            "test",
            "my-sonarr",
            StatusConditions {
                health: None,
                update: None,
                admin_creds: None,
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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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
            "test",
            "my-sonarr",
            StatusConditions {
                health: None,
                update: None,
                admin_creds: None,
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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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

        // GET secret for sonarr
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/sonarr-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": { "name": "sonarr-secret", "namespace": "test" },
                "data": { "api-key": "c29uYXJyLWtleQ==" }
            })))
            .mount(&mock_server)
            .await;

        // GET secret for radarr
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/radarr-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": { "name": "radarr-secret", "namespace": "test" },
                "data": { "api-key": "cmFkYXJyLWtleQ==" }
            })))
            .mount(&mock_server)
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
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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

        // GET secret for sonarr
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/test/secrets/sonarr-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": { "name": "sonarr-secret", "namespace": "test" },
                "data": { "api-key": "c29uYXJyLWtleQ==" }
            })))
            .mount(&mock_server)
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
}
