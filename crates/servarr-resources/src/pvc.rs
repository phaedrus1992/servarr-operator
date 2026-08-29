use k8s_openapi::api::core::v1::{
    PersistentVolumeClaim, PersistentVolumeClaimSpec, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use servarr_crds::{AppConfig, AppDefaults, PvcVolume, ServarrApp, SshMode};
use std::collections::BTreeMap;

use crate::common;

/// # Errors
///
/// Two independent causes:
/// - `app.spec.app` has no `image-defaults.toml` entry or an unrecognized security profile (see
///   [`AppDefaults::try_for_app`]). Not reachable for any real `ServarrApp`: `spec.app` is a
///   closed [`servarr_crds::AppType`] enum, and
///   `AppDefaults::validate_all()`/`validate_all_passes_for_every_app_type` (servarr-crds'
///   `defaults_tests.rs`) prove every variant resolves. Kept as a `Result` rather than an
///   infallible call so a future `AppType` variant added without a matching
///   `image-defaults.toml` entry fails loudly here (and in that test) instead of panicking.
/// - `resolve_persistence`'s mount-path-collision check. This one *is* reachable for a real
///   `ServarrApp`: the admission webhook that normally catches it at apply-time is optional
///   (`WEBHOOK_ENABLED`), so a CR created without it running -- or whose collision only appears
///   after an operator upgrade adds a new operator-reserved mount -- can still exist in-cluster.
pub fn build_all(app: &ServarrApp) -> Result<Vec<PersistentVolumeClaim>, String> {
    let defaults = AppDefaults::try_for_app(&app.spec.app)
        .inspect_err(|e| common::log_app_defaults_error(app, e))?;
    let persistence = defaults
        .resolve_persistence(app)
        .inspect_err(|e| common::log_app_defaults_error(app, e))?;

    let mut pvcs: Vec<PersistentVolumeClaim> = persistence
        .volumes
        .iter()
        .filter(|v| v.existing_claim_name.is_none())
        .map(|v| build_one(app, v))
        .collect();

    // Shell mode: one read-write PVC per user for persistent ~/.ssh state
    // (known_hosts, config, identity files).
    if let Some(AppConfig::SshBastion(ref sc)) = app.spec.app_config {
        for user in &sc.users {
            if user.mode == SshMode::Shell {
                pvcs.push(build_ssh_home_pvc(app, &user.name));
            }
        }
    }

    Ok(pvcs)
}

fn build_ssh_home_pvc(app: &ServarrApp, username: &str) -> PersistentVolumeClaim {
    PersistentVolumeClaim {
        metadata: common::metadata(app, &format!("ssh-home-{username}")),
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".into()]),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".into(),
                    Quantity("10Mi".into()),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn build_one(app: &ServarrApp, vol: &PvcVolume) -> PersistentVolumeClaim {
    let storage_class = if vol.storage_class.is_empty() {
        None
    } else {
        Some(vol.storage_class.clone())
    };

    PersistentVolumeClaim {
        metadata: common::metadata(app, &vol.name),
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec![vol.access_mode.clone()]),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".into(),
                    Quantity(vol.size.clone()),
                )])),
                ..Default::default()
            }),
            storage_class_name: storage_class,
            ..Default::default()
        }),
        ..Default::default()
    }
}
