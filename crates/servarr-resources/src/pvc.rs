use k8s_openapi::api::core::v1::{
    PersistentVolumeClaim, PersistentVolumeClaimSpec, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use servarr_crds::{AppConfig, AppDefaults, PersistenceSpec, PvcVolume, ServarrApp, SshMode};
use std::collections::BTreeMap;

use crate::common;

pub fn build_all(app: &ServarrApp) -> Vec<PersistentVolumeClaim> {
    let defaults = AppDefaults::for_app(&app.spec.app);
    let merged: PersistenceSpec;
    let persistence = match &app.spec.persistence {
        None => &defaults.persistence,
        Some(spec) => {
            merged = defaults.persistence.merge_with(spec);
            &merged
        }
    };

    let mut pvcs: Vec<PersistentVolumeClaim> = persistence
        .volumes
        .iter()
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

    pvcs
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
