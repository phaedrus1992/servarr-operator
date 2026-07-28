//! Property-based coverage for persistence resolution (#401), following up on
//! the example-based tests in `defaults_tests.rs` added by #386/#376.
//!
//! `find_mount_path_collision` is private, so its properties are exercised
//! through the public `AppDefaults::resolve_persistence` with an `AppDefaults`
//! built directly (not via `try_for_app`) so `persistence.volumes` starts
//! empty — the "restore any dropped default volume" step then has nothing to
//! restore, and `AppType::Sonarr` keeps `operator_reserved_mounts` empty. This
//! isolates the collision check from the rest of `resolve_persistence`'s
//! behavior without needing to make the helper `pub`.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use proptest::prelude::*;
use servarr_crds::*;
use std::collections::HashSet;

fn make_app(app_type: AppType) -> ServarrApp {
    ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: app_type,
            ..Default::default()
        },
        status: None,
    }
}

/// An `AppDefaults` whose compiled persistence is empty, so
/// `resolve_persistence`'s "restore dropped defaults" step is a no-op and the
/// result is exactly what the override contains (modulo tombstoning).
fn empty_defaults() -> AppDefaults {
    let mut defaults =
        AppDefaults::try_for_app(&AppType::Sonarr).expect("Sonarr defaults should load");
    defaults.persistence = PersistenceSpec::default();
    defaults
}

fn arb_mount_path() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("/a".to_string()),
        Just("/a/".to_string()),
        Just("/a//".to_string()),
        Just("/b".to_string()),
        Just("/b/".to_string()),
        Just("/c".to_string()),
    ]
}

fn arb_pvc_at(mount_path: String, name: String) -> PvcVolume {
    PvcVolume {
        name,
        mount_path,
        access_mode: "ReadWriteOnce".into(),
        size: "1Gi".into(),
        storage_class: String::new(),
        existing_claim_name: None,
    }
}

fn arb_nfs_at(mount_path: String, name: String) -> NfsMount {
    NfsMount {
        name,
        server: "nas.local".into(),
        path: "/export".into(),
        mount_path,
        read_only: false,
    }
}

/// Up to 5 entries (mixed PVC/NFS), each with a distinct generated name and a
/// mount path drawn from the small alphabet above — small enough that
/// collisions (including trailing-slash variants of the same path) occur
/// often in generated cases, exercising both the collision and no-collision
/// branches.
fn arb_entries() -> impl Strategy<Value = (Vec<PvcVolume>, Vec<NfsMount>)> {
    prop::collection::vec((any::<bool>(), arb_mount_path()), 0..5).prop_map(|entries| {
        let mut volumes = Vec::new();
        let mut nfs_mounts = Vec::new();
        for (i, (is_pvc, path)) in entries.into_iter().enumerate() {
            let name = format!("entry-{i}");
            if is_pvc {
                volumes.push(arb_pvc_at(path, name));
            } else {
                nfs_mounts.push(arb_nfs_at(path, name));
            }
        }
        (volumes, nfs_mounts)
    })
}

/// Mirrors defaults.rs's normalize_mount_path: same component-based
/// normalization, kept as an independent expression of the same intent
/// rather than importing the private helper.
fn normalize(path: &str) -> String {
    let mut normalized = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/");
    normalized.insert(0, '/');
    normalized
}

fn normalized_paths(volumes: &[PvcVolume], nfs_mounts: &[NfsMount]) -> Vec<String> {
    volumes
        .iter()
        .map(|v| normalize(&v.mount_path))
        .chain(nfs_mounts.iter().map(|m| normalize(&m.mount_path)))
        .collect()
}

fn has_collision(volumes: &[PvcVolume], nfs_mounts: &[NfsMount]) -> bool {
    let paths = normalized_paths(volumes, nfs_mounts);
    let unique: HashSet<&String> = paths.iter().collect();
    unique.len() != paths.len()
}

proptest! {
    /// Completeness: whenever the generated entries share a normalized mount
    /// path (including trailing-slash variants of the same path),
    /// `resolve_persistence` must reject them.
    #[test]
    fn collision_detected_whenever_paths_collide((volumes, nfs_mounts) in arb_entries()) {
        let defaults = empty_defaults();
        let mut app = make_app(AppType::Sonarr);
        app.spec.persistence = Some(PersistenceSpec {
            volumes: volumes.clone(),
            nfs_mounts: nfs_mounts.clone(),
            removed_default_volumes: vec![],
        });

        let result = defaults.resolve_persistence(&app);
        prop_assert_eq!(result.is_err(), has_collision(&volumes, &nfs_mounts));
    }

    /// `resolve_persistence` must not panic on arbitrary valid overrides,
    /// across every app type.
    #[test]
    fn resolve_persistence_never_panics(
        app_type in arb_app_type(),
        (volumes, nfs_mounts) in arb_entries(),
        removed_default_volumes in prop::collection::vec("[a-z]{1,8}", 0..3),
    ) {
        let defaults = AppDefaults::try_for_app(&app_type).expect("every AppType has defaults");
        let mut app = make_app(app_type);
        app.spec.persistence = Some(PersistenceSpec {
            volumes,
            nfs_mounts,
            removed_default_volumes,
        });

        let _ = defaults.resolve_persistence(&app);
    }

    /// Tombstone idempotence: listing a name twice in `removed_default_volumes`
    /// removes it exactly as listing it once would.
    #[test]
    fn tombstone_listed_twice_matches_listed_once(
        app_type in arb_app_type(),
    ) {
        let defaults = AppDefaults::try_for_app(&app_type).expect("every AppType has defaults");
        let Some(target) = defaults.persistence.volumes.first().map(|v| v.name.clone()) else {
            return Ok(());
        };

        let mut once = make_app(app_type.clone());
        once.spec.persistence = Some(PersistenceSpec {
            volumes: vec![],
            nfs_mounts: vec![],
            removed_default_volumes: vec![target.clone()],
        });
        let mut twice = make_app(app_type);
        twice.spec.persistence = Some(PersistenceSpec {
            volumes: vec![],
            nfs_mounts: vec![],
            removed_default_volumes: vec![target.clone(), target],
        });

        let once_names: Vec<String> = defaults
            .resolve_persistence(&once)
            .expect("tombstoning a real default must not error")
            .volumes
            .into_iter()
            .map(|v| v.name)
            .collect();
        let twice_names: Vec<String> = defaults
            .resolve_persistence(&twice)
            .expect("tombstoning a real default must not error")
            .volumes
            .into_iter()
            .map(|v| v.name)
            .collect();

        prop_assert_eq!(once_names, twice_names);
    }

    /// Override-precedence: explicitly re-listing a tombstoned default volume
    /// in `volumes` keeps it — an explicit entry always beats a tombstone.
    #[test]
    fn explicit_relist_beats_tombstone(app_type in arb_app_type()) {
        let defaults = AppDefaults::try_for_app(&app_type).expect("every AppType has defaults");
        let Some(target) = defaults.persistence.volumes.first().cloned() else {
            return Ok(());
        };

        let mut app = make_app(app_type);
        app.spec.persistence = Some(PersistenceSpec {
            volumes: vec![target.clone()],
            nfs_mounts: vec![],
            removed_default_volumes: vec![target.name.clone()],
        });

        let resolved = defaults
            .resolve_persistence(&app)
            .expect("no collision expected");
        prop_assert!(resolved.volumes.iter().any(|v| v.name == target.name));
    }

    /// `PersistenceSpec::merge_with`'s `removed_default_volumes` is a union of
    /// both layers, deduplicated — never a receiver-wins replace.
    #[test]
    fn merge_with_unions_removed_default_volumes(
        base_names in prop::collection::vec("[a-z]{1,6}", 0..4),
        override_names in prop::collection::vec("[a-z]{1,6}", 0..4),
    ) {
        let base = PersistenceSpec {
            volumes: vec![],
            nfs_mounts: vec![],
            removed_default_volumes: base_names.clone(),
        };
        let over = PersistenceSpec {
            volumes: vec![],
            nfs_mounts: vec![],
            removed_default_volumes: override_names.clone(),
        };

        let merged = over.merge_with(&base);
        let merged_set: HashSet<&String> = merged.removed_default_volumes.iter().collect();
        let expected_set: HashSet<&String> =
            base_names.iter().chain(override_names.iter()).collect();

        // Union must be deduplicated, not a raw concatenation.
        prop_assert_eq!(
            merged.removed_default_volumes.len(),
            merged_set.len(),
            "removed_default_volumes must be deduplicated"
        );
        prop_assert_eq!(merged_set, expected_set);
    }
}

fn arb_app_type() -> impl Strategy<Value = AppType> {
    prop_oneof![
        Just(AppType::Sonarr),
        Just(AppType::Radarr),
        Just(AppType::Lidarr),
        Just(AppType::Prowlarr),
        Just(AppType::Sabnzbd),
        Just(AppType::Transmission),
        Just(AppType::Tautulli),
        Just(AppType::Overseerr),
        Just(AppType::Maintainerr),
        Just(AppType::Jackett),
        Just(AppType::Jellyfin),
        Just(AppType::Plex),
        Just(AppType::SshBastion),
        Just(AppType::Bazarr),
        Just(AppType::Subgen),
    ]
}
