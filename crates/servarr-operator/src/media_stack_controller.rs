use chrono;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, Service};
use kube::api::{Api, DeleteParams, ListParams, ObjectList, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, Resource, ResourceExt};
use servarr_api::k8s::kube_err_summary;
use servarr_crds::{
    AppType, Condition, MediaStack, MediaStackStatus, ServarrApp, ServarrAppSpec, StackAppStatus,
    StackPhase,
};
use thiserror::Error;
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use crate::context::Context;
use crate::metrics::{
    increment_stack_reconcile_total, observe_stack_reconcile_duration, set_managed_stacks,
};

const FIELD_MANAGER: &str = "servarr-operator-stack";
const TIER_TIMEOUT_SECS: i64 = 300; // 5 minutes

#[derive(Debug, Error)]
pub enum Error {
    /// Message is already log-safe: [`kube_err_summary`] strips raw API server response content
    /// before this variant's `Display` ever renders it, so no separate sanitizer method is
    /// needed for logging.
    #[error("Kubernetes API error: {}", kube_err_summary(.0))]
    Kube(#[source] kube::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
    #[error("Internal error: {0}")]
    Internal(&'static str),
}

pub fn print_crd() -> Result<()> {
    let crd = servarr_crds::crd_with_legacy_field_aliases::<MediaStack>();
    let yaml = serde_yaml::to_string(&crd)?;
    println!("{yaml}");
    Ok(())
}

pub async fn run(client: kube::Client, server_state: crate::server::ServerState) -> Result<()> {
    let ctx = Arc::new(Context::new(client.clone()));

    let (stacks, apps) = if let Some(ref ns) = ctx.watch_namespace {
        (
            Api::<MediaStack>::namespaced(client.clone(), ns),
            Api::<ServarrApp>::namespaced(client.clone(), ns),
        )
    } else {
        (
            Api::<MediaStack>::all(client.clone()),
            Api::<ServarrApp>::all(client.clone()),
        )
    };

    info!("Starting media-stack controller");
    server_state.set_ready();

    Controller::new(stacks, watcher::Config::default())
        .owns(apps, watcher::Config::default())
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "media-stack reconciled"),
                Err(e) => error!(%e, "media-stack reconcile error"),
            }
        })
        .await;

    Ok(())
}

pub async fn reconcile(stack: Arc<MediaStack>, ctx: Arc<Context>) -> Result<Action, Error> {
    let client = &ctx.client;
    let name = stack.name_any();
    let ns = stack.namespace().unwrap_or_else(|| "default".into());
    let pp = PatchParams::apply(FIELD_MANAGER).force();

    info!(%name, %ns, "reconciling MediaStack");
    let start_time = std::time::Instant::now();

    let defaults = stack.spec.defaults.as_ref();

    // Reconcile in-cluster NFS server StatefulSet and Service.
    // Returns the pod IP if the server is running (used below to bypass cluster DNS).
    let nfs_pod_ip = reconcile_nfs_server(&stack, client, &name, &ns, &pp).await?;

    // Build an effective NfsServerSpec: for in-cluster servers override the server
    // address with the pod IP so the kubelet can resolve it without cluster DNS.
    let nfs_override: Option<servarr_crds::NfsServerSpec>;
    let effective_nfs = match (stack.spec.nfs.as_ref(), nfs_pod_ip) {
        (Some(nfs), Some(ip)) if nfs.external_server.is_none() => {
            nfs_override = Some(servarr_crds::NfsServerSpec {
                external_server: Some(ip),
                external_path: String::new(),
                ..nfs.clone()
            });
            nfs_override.as_ref()
        }
        _ => stack.spec.nfs.as_ref(),
    };

    // Collect enabled apps and expand split4k entries
    let mut expanded: Vec<(String, ServarrAppSpec, AppType, u8)> = Vec::new();
    for app in stack.spec.apps.iter().filter(|a| a.enabled) {
        match app.expand(&name, &ns, defaults, effective_nfs) {
            Ok(pairs) => {
                for (child_name, spec) in pairs {
                    let tier = app.app.tier();
                    let app_type = spec.app.clone();
                    expanded.push((child_name, spec, app_type, tier));
                }
            }
            Err(msg) => {
                warn!(%name, error = %msg, "split4k validation failed");
                let now = chrono_now();
                let mut status = MediaStackStatus::default();
                status.set_condition(Condition::fail("Valid", "InvalidSplit4k", &msg, &now));
                status.observed_generation = stack.metadata.generation.unwrap_or(0);
                patch_status(client, &ns, &name, &status).await?;
                increment_stack_reconcile_total("error");
                return Ok(Action::requeue(Duration::from_secs(60)));
            }
        }
    }

    // Check for duplicate child names
    {
        let mut seen = HashSet::new();
        for (child_name, _, _, _) in &expanded {
            if !seen.insert(child_name.clone()) {
                warn!(%name, child = %child_name, "duplicate app+instance in MediaStack");
                let now = chrono_now();
                let mut status = MediaStackStatus::default();
                status.set_condition(Condition::fail(
                    "Valid",
                    "DuplicateApp",
                    &format!("Duplicate app+instance: {child_name}"),
                    &now,
                ));
                status.observed_generation = stack.metadata.generation.unwrap_or(0);
                patch_status(client, &ns, &name, &status).await?;
                increment_stack_reconcile_total("error");
                return Ok(Action::requeue(Duration::from_secs(60)));
            }
        }
    }

    // Group by tier
    let mut tiers: BTreeMap<u8, Vec<(String, ServarrAppSpec, AppType)>> = BTreeMap::new();
    for (child_name, spec, app_type, tier) in expanded {
        tiers
            .entry(tier)
            .or_default()
            .push((child_name, spec, app_type));
    }

    // Desired child names for orphan cleanup
    let desired_children: HashSet<String> = tiers
        .values()
        .flat_map(|apps| apps.iter().map(|(n, _, _)| n.clone()))
        .collect();

    let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);

    // Cleanup orphaned children BEFORE applying desired children. A renamed app (e.g.
    // Overseerr -> Seerr) produces a new child name whose normalized `spec.app` collides
    // with the stale orphan's under the admission webhook's duplicate-instance check.
    // Deleting the orphan first avoids that collision; deleting it after the apply loop
    // (as this used to) means every apply is rejected and cleanup is never reached,
    // permanently wedging the stack (#533).
    let label_selector = format!("servarr.dev/stack={name}");
    let existing = sa_api
        .list(&ListParams::default().labels(&label_selector))
        .await
        .map_err(Error::Kube)?;

    let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), &ns);
    let stuck_orphans =
        cleanup_orphaned_children(&sa_api, &pvc_api, &name, &desired_children, &existing).await;

    let mut app_statuses: Vec<StackAppStatus> = Vec::new();
    let mut ready_count: i32 = 0;
    let mut current_tier: Option<u8> = None;
    let mut all_previous_ready = true;

    // Previous-reconcile state needed for tier timeout logic.
    let prev_current_tier = stack.status.as_ref().and_then(|s| s.current_tier);
    let prev_tier_blocked_since = stack
        .status
        .as_ref()
        .and_then(|s| s.tier_blocked_since.clone());
    let prev_bypassed: HashSet<String> = stack
        .status
        .as_ref()
        .map(|s| {
            s.app_statuses
                .iter()
                .filter(|a| a.bypassed)
                .map(|a| a.name.clone())
                .collect()
        })
        .unwrap_or_default();

    // Iterate tiers in order
    for (&tier, apps) in &tiers {
        if tier > 0 && !all_previous_ready {
            // Check if we should advance past this block due to timeout.
            let timed_out = prev_tier_blocked_since
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|since| {
                    use chrono::Utc;
                    (Utc::now() - since.with_timezone(&Utc)).num_seconds() >= TIER_TIMEOUT_SECS
                });

            if !timed_out {
                // Previous tier not ready and not timed out — skip.
                for (child_name, _, app_type) in apps {
                    app_statuses.push(StackAppStatus {
                        name: child_name.clone(),
                        app_type: app_type.as_str().to_string(),
                        tier,
                        ready: false,
                        enabled: true,
                        bypassed: false,
                    });
                }
                continue;
            }

            // Timeout elapsed: mark all currently-unready apps as bypassed so
            // subsequent reconciles don't re-block on them.
            warn!(
                %name, tier,
                timeout_secs = TIER_TIMEOUT_SECS,
                "tier rollout timed out; advancing past unready apps"
            );
            for status in app_statuses.iter_mut() {
                if !status.ready && !status.bypassed {
                    status.bypassed = true;
                }
            }
            all_previous_ready = true;
        }

        current_tier = Some(tier);

        for (child_name, spec, app_type) in apps {
            // Build child ServarrApp with ownerReferences and labels
            let owner_ref = stack
                .controller_owner_ref(&())
                .expect("stack should have UID");

            let child = ServarrApp::new(child_name, spec.clone());
            let mut child_value = serde_json::to_value(&child).map_err(Error::Serialization)?;

            // Inject metadata. serde_json::to_value on a struct always produces
            // an object, so these casts are guaranteed by the type system.
            let child_obj = child_value.as_object_mut().ok_or(Error::Internal(
                "serialized ServarrApp is not a JSON object",
            ))?;
            let meta = child_obj
                .entry("metadata")
                .or_insert_with(|| serde_json::json!({}));
            let meta_obj = meta
                .as_object_mut()
                .ok_or(Error::Internal("metadata field is not a JSON object"))?;
            meta_obj.insert("namespace".to_string(), serde_json::json!(ns));
            meta_obj.insert(
                "ownerReferences".to_string(),
                serde_json::to_value(vec![&owner_ref]).map_err(Error::Serialization)?,
            );
            meta_obj.insert(
                "labels".to_string(),
                serde_json::json!({
                    "servarr.dev/stack": name,
                    "servarr.dev/tier": tier.to_string(),
                    "app.kubernetes.io/managed-by": FIELD_MANAGER
                }),
            );

            sa_api
                .patch(child_name, &pp, &Patch::Apply(child_value))
                .await
                .map_err(Error::Kube)?;

            // Read back child status.  Bypassed apps count as "ready" for tier
            // advancement but the actual ready flag is preserved in status.
            let was_bypassed = prev_bypassed.contains(child_name.as_str());
            let actual_ready = match sa_api.get(child_name).await {
                Ok(sa) => sa.status.as_ref().is_some_and(|s| s.ready),
                Err(_) => false,
            };

            if actual_ready {
                ready_count += 1;
            }
            if !actual_ready && !was_bypassed {
                all_previous_ready = false;
            }

            app_statuses.push(StackAppStatus {
                name: child_name.clone(),
                app_type: app_type.as_str().to_string(),
                tier,
                ready: actual_ready,
                enabled: true,
                // Clear bypass once the app is actually ready.
                bypassed: was_bypassed && !actual_ready,
            });
        }
    }

    // Add disabled apps to statuses
    for app in &stack.spec.apps {
        if !app.enabled {
            app_statuses.push(StackAppStatus {
                name: app.child_name(&name),
                app_type: app.app.as_str().to_string(),
                tier: app.app.tier(),
                ready: false,
                enabled: false,
                bypassed: false,
            });
        }
    }

    // Compute phase (total_apps is the expanded count)
    let total_apps = desired_children.len() as i32;
    let was_ready = stack
        .status
        .as_ref()
        .is_some_and(|s| s.phase == StackPhase::Ready);

    let phase = if total_apps == 0 {
        StackPhase::Pending
    } else if ready_count == total_apps {
        StackPhase::Ready
    } else if was_ready && ready_count < total_apps {
        StackPhase::Degraded
    } else if ready_count > 0 {
        StackPhase::RollingOut
    } else {
        StackPhase::Pending
    };

    let now = chrono_now();

    // Maintain tier_blocked_since: set when this tier first blocks, reset when
    // the tier advances or all apps become ready.
    let tier_blocked_since = if ready_count == total_apps {
        None
    } else if current_tier != prev_current_tier {
        // Tier advanced — start a fresh timer for the new tier.
        Some(now.clone())
    } else if !all_previous_ready {
        // Same tier, still blocked — preserve existing timer or start it.
        prev_tier_blocked_since.or_else(|| Some(now.clone()))
    } else {
        None
    };

    let mut status = MediaStackStatus {
        ready: phase == StackPhase::Ready,
        phase: phase.clone(),
        current_tier,
        total_apps,
        ready_apps: ready_count,
        app_statuses,
        conditions: Vec::new(),
        observed_generation: stack.metadata.generation.unwrap_or(0),
        tier_blocked_since,
    };

    status.set_condition(Condition::ok("Valid", "Valid", "Spec is valid", &now));

    if stuck_orphans.is_empty() {
        status.set_condition(Condition::ok(
            "OrphanCleanupHealthy",
            "NoStuckOrphans",
            "no orphaned children awaiting PVC detach",
            &now,
        ));
    } else {
        // #610: distinguish a permission denial (won't self-resolve without a manual fix) from
        // a transient failure (may clear on its own next reconcile) so on-call doesn't have to
        // guess from operator logs alone. "Permission fix" rather than "RBAC fix": a 401/403 can
        // also come from an expired credential, a ResourceQuota, or an admission policy, not
        // only a Role/RoleBinding.
        let mut causes = Vec::new();
        if !stuck_orphans.forbidden.is_empty() {
            causes.push(format!(
                "{} awaiting a manual permission fix (will not self-resolve on retry): {}",
                stuck_orphans.forbidden.len(),
                stuck_orphans.forbidden.join(", ")
            ));
        }
        if !stuck_orphans.transient.is_empty() {
            causes.push(format!(
                "{} awaiting retry (may self-resolve): {}",
                stuck_orphans.transient.len(),
                stuck_orphans.transient.join(", ")
            ));
        }
        status.set_condition(Condition::fail(
            "OrphanCleanupHealthy",
            "PvcDetachFailed",
            &format!(
                "{} orphaned child(ren) awaiting PVC detach before cleanup can proceed: {}",
                stuck_orphans.len(),
                causes.join("; ")
            ),
            &now,
        ));
    }

    match &phase {
        StackPhase::Ready => {
            status.set_condition(Condition::ok(
                "Ready",
                "AllAppsReady",
                &format!("{ready_count}/{total_apps} apps ready"),
                &now,
            ));
        }
        StackPhase::RollingOut => {
            status.set_condition(Condition::fail(
                "Ready",
                "RollingOut",
                &format!(
                    "{ready_count}/{total_apps} apps ready, rolling out tier {}",
                    current_tier.unwrap_or(0)
                ),
                &now,
            ));
        }
        StackPhase::Degraded => {
            status.set_condition(Condition::fail(
                "Ready",
                "Degraded",
                &format!("{ready_count}/{total_apps} apps ready (was fully ready)"),
                &now,
            ));
        }
        StackPhase::Pending => {
            status.set_condition(Condition::fail(
                "Ready",
                "Pending",
                "No apps ready yet",
                &now,
            ));
        }
    }

    patch_status(client, &ns, &name, &status).await?;

    // Update managed-stacks gauge
    let gauge_api = if let Some(ref ns) = ctx.watch_namespace {
        Api::<MediaStack>::namespaced(client.clone(), ns)
    } else {
        Api::<MediaStack>::all(client.clone())
    };
    if let Ok(stack_list) = gauge_api.list(&ListParams::default()).await {
        let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for s in &stack_list.items {
            let key = s.namespace().unwrap_or_default();
            *counts.entry(key).or_default() += 1;
        }
        for (ns_key, count) in &counts {
            set_managed_stacks(ns_key, *count);
        }
    }

    let duration = start_time.elapsed().as_secs_f64();
    observe_stack_reconcile_duration(duration);
    increment_stack_reconcile_total("success");

    info!(%name, %phase, ready = ready_count, total = total_apps, "MediaStack reconciliation complete");

    // Requeue interval based on phase
    let requeue = match phase {
        StackPhase::Ready => Duration::from_secs(300),
        _ => Duration::from_secs(30),
    };

    Ok(Action::requeue(requeue))
}

/// Apply (or clean up) the in-cluster NFS server StatefulSet and Service.
///
/// When `nfs.deploy_in_cluster()` is true, both resources are created/updated
/// via server-side apply. When NFS is absent or external, any previously-created
/// in-cluster resources are deleted so they don't linger.
///
/// Returns the NFS server pod IP if the pod is currently running, or `None`
/// if the pod is not yet scheduled/running. Callers should use this IP directly
/// instead of the service hostname because the kubelet mounts volumes from the
/// host network namespace where cluster-internal DNS may not be available.
async fn reconcile_nfs_server(
    stack: &MediaStack,
    client: &Client,
    name: &str,
    ns: &str,
    pp: &PatchParams,
) -> Result<Option<String>, Error> {
    let nfs_name = format!("{name}-nfs-server");
    let ss_api = Api::<StatefulSet>::namespaced(client.clone(), ns);
    let svc_api = Api::<Service>::namespaced(client.clone(), ns);

    let deploy = stack
        .spec
        .nfs
        .as_ref()
        .is_some_and(|n| n.deploy_in_cluster());

    if deploy {
        let nfs = stack.spec.nfs.as_ref().expect("checked above");
        let owner_ref = stack
            .controller_owner_ref(&())
            .expect("stack should have UID");

        let statefulset =
            servarr_resources::nfs_server::build_statefulset(name, ns, nfs, owner_ref.clone());
        let service = servarr_resources::nfs_server::build_service(name, ns, owner_ref);

        ss_api
            .patch(
                &nfs_name,
                pp,
                &Patch::Apply(serde_json::to_value(&statefulset).map_err(Error::Serialization)?),
            )
            .await
            .map_err(Error::Kube)?;

        svc_api
            .patch(
                &nfs_name,
                pp,
                &Patch::Apply(serde_json::to_value(&service).map_err(Error::Serialization)?),
            )
            .await
            .map_err(Error::Kube)?;

        info!(%name, %ns, "applied NFS server StatefulSet and Service");

        // Look up the pod IP of the running NFS server pod.  The kubelet mounts
        // volumes from the host network namespace where cluster-internal DNS may
        // not resolve.  Using the pod IP directly bypasses that limitation.
        let pod_api = Api::<Pod>::namespaced(client.clone(), ns);
        let pod_name = format!("{nfs_name}-0");
        let pod_ip = pod_api
            .get_opt(&pod_name)
            .await
            .map_err(Error::Kube)?
            .and_then(|p| p.status?.pod_ip);

        if let Some(ref ip) = pod_ip {
            info!(%name, %ns, pod_ip = %ip, "NFS server pod IP resolved");
        } else {
            info!(%name, %ns, "NFS server pod not yet running; will retry");
        }

        return Ok(pod_ip);
    }

    // NFS disabled or external — remove any in-cluster resources.
    for result in [
        ss_api
            .delete(&nfs_name, &DeleteParams::default())
            .await
            .map(|_| ()),
        svc_api
            .delete(&nfs_name, &DeleteParams::default())
            .await
            .map(|_| ()),
    ] {
        if let Err(e) = result
            && !is_not_found(&e)
        {
            warn!(%name, error = %kube_err_summary(&e), "failed to delete NFS server resource");
        }
    }

    Ok(None)
}

/// Whether `e` proves the target the caller just tried to patch/delete is already gone, so the
/// caller can drop it silently instead of warning.
///
/// Deliberately does **not** adopt `controller::cleanup::ClassifyCleanupSeverity` (see #659):
/// that trait's Terminal/Transient duality exists to decide whether a finalizer keeps the CR
/// around for a retry, with a Warning Event published on the Transient path. Every call site
/// here is a best-effort orphan-cleanup step during normal reconciliation (detach a PVC owner
/// ref, delete an orphaned child/NFS resource) — there's no finalizer, no retry decision to
/// make, and no Event to publish; a non-404 error just gets logged and reconciliation moves on.
/// Sharing the underlying 404 check (not the retry semantics built on it) is enough — see
/// `servarr_api::k8s::is_kube_not_found`.
fn is_not_found(e: &kube::Error) -> bool {
    servarr_api::k8s::is_kube_not_found(e)
}

/// Why a PVC-detach step failed, for on-call triage (#610): a permission denial needs a manual
/// fix and will never clear on retry, while a transient error (5xx, network) may resolve on its
/// own next reconcile. On-call can't tell these apart from an identical `warn!` +
/// forever-retry line alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetachFailureCause {
    /// A `401`/`403` API response, or a `kube::Error::Auth` (credential/exec-plugin) failure.
    Forbidden,
    /// Any other failure (5xx, network, an unreachable PVC spec).
    Transient,
}

impl DetachFailureCause {
    fn from_kube_error(e: &kube::Error) -> Self {
        // Shared with cleanup.rs's RetryOutlook (#669) via servarr_api::k8s -- see that
        // function's doc comment for why a PVC-detach PATCH only needs this narrower check
        // rather than cleanup.rs's fuller classification.
        if servarr_api::k8s::is_kube_permission_denied(e) {
            Self::Forbidden
        } else {
            Self::Transient
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Forbidden => "permission-denied",
            Self::Transient => "transient",
        }
    }

    /// A child can hit more than one failure cause across its PVCs -- prefer `Forbidden` since
    /// it's the more actionable signal (won't self-resolve on retry the way a transient one
    /// might).
    fn most_severe(self, other: Self) -> Self {
        if self == Self::Forbidden || other == Self::Forbidden {
            Self::Forbidden
        } else {
            Self::Transient
        }
    }
}

/// Names of orphaned children whose PVC detach didn't unambiguously succeed this reconcile,
/// split by whether the failure is expected to self-resolve on retry (#610).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[must_use]
struct StuckOrphans {
    /// Blocked on a `401`/`403`/auth failure -- will not self-resolve without a permission fix.
    forbidden: Vec<String>,
    /// Blocked on any other failure -- may self-resolve on retry.
    transient: Vec<String>,
}

impl StuckOrphans {
    fn is_empty(&self) -> bool {
        self.forbidden.is_empty() && self.transient.is_empty()
    }

    fn len(&self) -> usize {
        self.forbidden.len() + self.transient.len()
    }
}

/// Deletes `ServarrApp` children no longer present in `desired_children`, first detaching their
/// PVC ownership so cascading GC on the CR delete doesn't take the PVC (and the user's config
/// data) down with it. Returns the children whose PVC detach didn't unambiguously succeed this
/// reconcile -- callers must surface these via status so a stuck orphan is visible via
/// `kubectl describe`/status instead of only a pod log line (#562 follow-up).
///
/// Returns `StuckOrphans`, which is itself `#[must_use]` -- that alone makes discarding this
/// call a warning, so the function isn't separately annotated (clippy's `double_must_use`).
async fn cleanup_orphaned_children(
    sa_api: &Api<ServarrApp>,
    pvc_api: &Api<PersistentVolumeClaim>,
    name: &str,
    desired_children: &HashSet<String>,
    existing: &ObjectList<ServarrApp>,
) -> StuckOrphans {
    let mut stuck_orphans = StuckOrphans::default();
    for child in existing {
        let child_name = child.name_any();
        if desired_children.contains(&child_name) {
            continue;
        }
        info!(%name, child = %child_name, "deleting orphaned child ServarrApp");
        // Detach this child's PVCs from the ownership chain before deleting the CR.
        // The child's config PVC is owned by it via ownerReference
        // (servarr_resources::common::metadata), and the default cascading delete
        // below would garbage-collect it along with the CR. A renamed app (e.g.
        // Overseerr -> Seerr within a MediaStack) produces a new child name, so this
        // delete now runs -- and succeeds -- on every such rename; it must never
        // silently destroy the user's config data as a side effect of routine
        // cleanup. Everything *else* the child owns (Deployment, Service, Secrets,
        // NetworkPolicy, routes) should still be torn down normally -- a
        // removed/renamed app must actually stop running, not linger unmanaged.
        let mut detach_failure: Option<DetachFailureCause> = None;
        match servarr_resources::pvc::build_all(child) {
            Ok(pvcs) => {
                for pvc in &pvcs {
                    let Some(pvc_name) = pvc.metadata.name.as_deref() else {
                        continue;
                    };
                    let detach = serde_json::json!({ "metadata": { "ownerReferences": null } });
                    match pvc_api
                        .patch(pvc_name, &PatchParams::default(), &Patch::Merge(detach))
                        .await
                    {
                        Ok(_) => {}
                        Err(e) if is_not_found(&e) => {
                            // #670: a 404 here is ambiguous by construction -- it could mean
                            // the PVC was already detached (a prior reconcile's patch already
                            // succeeded, or the PVC was deleted out-of-band), or it could mean
                            // `pvc::build_all` computed a name that never matched any real PVC
                            // (a naming-convention drift between it and the PVC's actual
                            // creation path). Neither blocks cleanup today, but this line gives
                            // on-call a breadcrumb: a PVC name that should exist showing up
                            // here repeatedly is a signal to check
                            // `servarr_resources::pvc`/`common::child_name` for drift.
                            debug!(
                                %name, child = %child_name, pvc = %pvc_name,
                                "PVC ownerReference detach returned 404 -- already \
                                 detached/deleted, or the computed PVC name never matched a \
                                 real PVC"
                            );
                        }
                        Err(e) => {
                            let cause = DetachFailureCause::from_kube_error(&e);
                            detach_failure =
                                Some(detach_failure.map_or(cause, |prev| prev.most_severe(cause)));
                            warn!(
                                %name, child = %child_name, pvc = %pvc_name,
                                error = %kube_err_summary(&e), cause = cause.as_str(),
                                "failed to detach PVC ownership before orphan cleanup"
                            );
                        }
                    }
                }
            }
            Err(e) => {
                // Not a `kube::Error` (see `pvc::build_all`'s docs on why this branch is
                // provably unreachable for any real `ServarrApp`) -- kept as `Transient` since
                // there's no RBAC status code to classify.
                detach_failure = Some(DetachFailureCause::Transient);
                warn!(
                    %name, child = %child_name, error = %e,
                    "failed to compute child's PVCs before orphan cleanup"
                );
            }
        }
        // Detaching every PVC didn't unambiguously succeed -- deleting the child now
        // would let cascading GC take a still-owned PVC down with it. Leave the child
        // as an orphan; it gets retried on the next reconcile.
        if let Some(cause) = detach_failure {
            warn!(
                %name, child = %child_name, cause = cause.as_str(),
                "skipping orphaned child delete this reconcile; will retry PVC detach next reconcile"
            );
            match cause {
                DetachFailureCause::Forbidden => stuck_orphans.forbidden.push(child_name.clone()),
                DetachFailureCause::Transient => stuck_orphans.transient.push(child_name.clone()),
            }
            continue;
        }
        if let Err(e) = sa_api.delete(&child_name, &Default::default()).await
            && !is_not_found(&e)
        {
            warn!(
                %name,
                child = %child_name,
                error = %kube_err_summary(&e),
                "failed to delete orphaned child"
            );
        }
    }
    stuck_orphans
}

async fn patch_status(
    client: &Client,
    ns: &str,
    name: &str,
    status: &MediaStackStatus,
) -> Result<(), Error> {
    let stacks = Api::<MediaStack>::namespaced(client.clone(), ns);
    let status_patch = serde_json::json!({
        "apiVersion": "servarr.dev/v1alpha1",
        "kind": "MediaStack",
        "status": status,
    });
    stacks
        .patch_status(
            name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(status_patch),
        )
        .await
        .map_err(Error::Kube)?;
    Ok(())
}

pub fn error_policy(_stack: Arc<MediaStack>, error: &Error, _ctx: Arc<Context>) -> Action {
    increment_stack_reconcile_total("error");
    warn!(error = %error, "media-stack reconciliation failed, requeuing");
    Action::requeue(Duration::from_secs(60))
}

fn chrono_now() -> String {
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_crd_returns_ok() {
        assert!(print_crd().is_ok());
    }

    #[test]
    fn error_display_kube_variant_drops_message_keeps_status_code() {
        let status = kube::core::Status {
            code: 403,
            message: "secrets \"super-secret-name\" is forbidden: User cannot get".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        };
        let err = Error::Kube(kube::Error::Api(Box::new(status)));
        let summary = err.to_string();
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
    fn chrono_now_returns_valid_iso8601() {
        let now = chrono_now();
        assert!(now.contains('T'), "should contain T separator: {now}");
        assert!(now.ends_with('Z'), "should end with Z: {now}");
    }

    // ------------------------------------------------------------------
    // cleanup_orphaned_children / DetachFailureCause (#549, #610)
    //
    // Unit-testable independently of the full reconcile wiremock harness: only the two API
    // calls this function itself makes (PVC PATCH, ServarrApp DELETE) are mocked -- no
    // MediaStack apply, child apply/get, or gauge list.
    // ------------------------------------------------------------------

    fn make_orphan(name: &str, ns: &str) -> ServarrApp {
        let mut app = ServarrApp::new(
            name,
            ServarrAppSpec {
                app: AppType::Radarr,
                ..Default::default()
            },
        );
        app.metadata.namespace = Some(ns.to_string());
        // `pvc::build_all` -> `common::owner_reference` needs a UID to build the
        // ownerReference.
        app.metadata.uid = Some("test-uid".to_string());
        app
    }

    fn orphan_list(orphans: Vec<ServarrApp>) -> ObjectList<ServarrApp> {
        ObjectList {
            types: Default::default(),
            metadata: Default::default(),
            items: orphans,
        }
    }

    #[tokio::test]
    async fn cleanup_orphaned_children_deletes_child_when_detach_succeeds() {
        let mock_server = wiremock::MockServer::start().await;
        let client = crate::testutils::build_mock_client(&mock_server.uri()).await;
        let sa_api = Api::<ServarrApp>::namespaced(client.clone(), "test");
        let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), "test");

        // Unmocked PVC PATCH -> wiremock's default 404, which `is_not_found` treats as
        // "already detached", not a failure.
        wiremock::Mock::given(wiremock::matchers::method("DELETE"))
            .and(wiremock::matchers::path(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/orphan-app",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "metadata": { "name": "orphan-app", "namespace": "test" }
                })),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let existing = orphan_list(vec![make_orphan("orphan-app", "test")]);
        let desired: HashSet<String> = HashSet::new();

        let stuck =
            cleanup_orphaned_children(&sa_api, &pvc_api, "stack", &desired, &existing).await;

        assert!(
            stuck.is_empty(),
            "no PVC-detach failure -> no stuck orphans, got {stuck:?}"
        );
        // The DELETE mock's expect(1) is verified when mock_server drops.
    }

    #[tokio::test]
    async fn cleanup_orphaned_children_classifies_403_as_forbidden() {
        let mock_server = wiremock::MockServer::start().await;
        let client = crate::testutils::build_mock_client(&mock_server.uri()).await;
        let sa_api = Api::<ServarrApp>::namespaced(client.clone(), "test");
        let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), "test");

        wiremock::Mock::given(wiremock::matchers::method("PATCH"))
            .and(wiremock::matchers::path(
                "/api/v1/namespaces/test/persistentvolumeclaims/orphan-app-config",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(403).set_body_json(serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "Status",
                    "status": "Failure",
                    "reason": "Forbidden",
                    "code": 403
                })),
            )
            .mount(&mock_server)
            .await;

        let existing = orphan_list(vec![make_orphan("orphan-app", "test")]);
        let desired: HashSet<String> = HashSet::new();

        let stuck =
            cleanup_orphaned_children(&sa_api, &pvc_api, "stack", &desired, &existing).await;

        assert_eq!(stuck.forbidden, vec!["orphan-app".to_string()]);
        assert!(stuck.transient.is_empty(), "got {stuck:?}");
    }

    #[tokio::test]
    async fn cleanup_orphaned_children_classifies_401_as_forbidden() {
        // #610 review: an earlier draft classified only 403, silently treating an expired or
        // rotated ServiceAccount token (401) as "may self-resolve on retry" when it never will.
        let mock_server = wiremock::MockServer::start().await;
        let client = crate::testutils::build_mock_client(&mock_server.uri()).await;
        let sa_api = Api::<ServarrApp>::namespaced(client.clone(), "test");
        let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), "test");

        wiremock::Mock::given(wiremock::matchers::method("PATCH"))
            .and(wiremock::matchers::path(
                "/api/v1/namespaces/test/persistentvolumeclaims/orphan-app-config",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(401).set_body_json(serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "Status",
                    "status": "Failure",
                    "reason": "Unauthorized",
                    "code": 401
                })),
            )
            .mount(&mock_server)
            .await;

        let existing = orphan_list(vec![make_orphan("orphan-app", "test")]);
        let desired: HashSet<String> = HashSet::new();

        let stuck =
            cleanup_orphaned_children(&sa_api, &pvc_api, "stack", &desired, &existing).await;

        assert_eq!(stuck.forbidden, vec!["orphan-app".to_string()]);
        assert!(stuck.transient.is_empty(), "got {stuck:?}");
    }

    #[tokio::test]
    async fn cleanup_orphaned_children_classifies_500_as_transient() {
        let mock_server = wiremock::MockServer::start().await;
        let client = crate::testutils::build_mock_client(&mock_server.uri()).await;
        let sa_api = Api::<ServarrApp>::namespaced(client.clone(), "test");
        let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), "test");

        wiremock::Mock::given(wiremock::matchers::method("PATCH"))
            .and(wiremock::matchers::path(
                "/api/v1/namespaces/test/persistentvolumeclaims/orphan-app-config",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(500).set_body_json(serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "Status",
                    "status": "Failure",
                    "reason": "InternalError",
                    "code": 500
                })),
            )
            .mount(&mock_server)
            .await;

        let existing = orphan_list(vec![make_orphan("orphan-app", "test")]);
        let desired: HashSet<String> = HashSet::new();

        let stuck =
            cleanup_orphaned_children(&sa_api, &pvc_api, "stack", &desired, &existing).await;

        assert_eq!(stuck.transient, vec!["orphan-app".to_string()]);
        assert!(stuck.forbidden.is_empty(), "got {stuck:?}");
    }

    #[test]
    fn detach_failure_cause_most_severe_prefers_forbidden() {
        assert_eq!(
            DetachFailureCause::Transient.most_severe(DetachFailureCause::Forbidden),
            DetachFailureCause::Forbidden
        );
        assert_eq!(
            DetachFailureCause::Forbidden.most_severe(DetachFailureCause::Transient),
            DetachFailureCause::Forbidden
        );
        assert_eq!(
            DetachFailureCause::Transient.most_severe(DetachFailureCause::Transient),
            DetachFailureCause::Transient
        );
    }

    mod stuck_orphans_proptests {
        use super::*;
        use proptest::prelude::*;

        fn any_cause() -> impl Strategy<Value = DetachFailureCause> {
            proptest::sample::select(&[
                DetachFailureCause::Forbidden,
                DetachFailureCause::Transient,
            ])
        }

        proptest! {
            #[test]
            fn most_severe_is_commutative_and_idempotent(a in any_cause(), b in any_cause()) {
                prop_assert_eq!(a.most_severe(b), b.most_severe(a));
                prop_assert_eq!(a.most_severe(a), a);
            }

            #[test]
            fn len_and_is_empty_agree(
                forbidden in proptest::collection::vec("[a-z-]{1,8}", 0..4),
                transient in proptest::collection::vec("[a-z-]{1,8}", 0..4),
            ) {
                let stuck = StuckOrphans { forbidden: forbidden.clone(), transient: transient.clone() };
                prop_assert_eq!(stuck.len(), forbidden.len() + transient.len());
                // Deliberately comparing `len() == 0` against `is_empty()` -- this is the
                // invariant under test, not a style choice `is_empty()` would make tautological.
                #[expect(clippy::len_zero, reason = "cross-checking is_empty() against len()")]
                let len_is_zero = stuck.len() == 0;
                prop_assert_eq!(stuck.is_empty(), len_is_zero);
            }
        }
    }
}
