use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use servarr_crds::*;

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

// ---------------------------------------------------------------------------
// SSH Bastion defaults
// ---------------------------------------------------------------------------

#[test]
fn ssh_bastion_uses_custom_security_profile() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert!(matches!(
        defaults.security.profile_type,
        SecurityProfileType::Custom
    ));
}

#[test]
fn ssh_bastion_runs_as_root() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert_eq!(defaults.security.user, 0);
    assert_eq!(defaults.security.group, 0);
    assert_eq!(defaults.uid, 0);
    assert_eq!(defaults.gid, 0);
}

#[test]
fn ssh_bastion_has_required_capabilities() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    let caps = &defaults.security.capabilities_add;

    let required = [
        "CHOWN",
        "SETGID",
        "SETUID",
        "NET_BIND_SERVICE",
        "SYS_CHROOT",
    ];
    for cap in &required {
        assert!(caps.iter().any(|c| c == cap), "missing capability: {cap}");
    }
    assert_eq!(caps.len(), required.len(), "unexpected extra capabilities");
}

#[test]
fn ssh_bastion_drops_all_capabilities() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert_eq!(defaults.security.capabilities_drop, vec!["ALL".to_string()]);
}

#[test]
fn ssh_bastion_security_flags() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert_eq!(defaults.security.run_as_non_root, Some(false));
    assert_eq!(defaults.security.read_only_root_filesystem, Some(false));
    assert_eq!(defaults.security.allow_privilege_escalation, Some(false));
}

#[test]
fn ssh_bastion_service_port_is_ssh() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert_eq!(defaults.service.ports.len(), 1);
    assert_eq!(defaults.service.ports[0].name, "ssh");
    assert_eq!(defaults.service.ports[0].protocol, "TCP");
    assert_eq!(defaults.service.service_type, "ClusterIP");
}

#[test]
fn ssh_bastion_has_host_keys_volume() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert_eq!(defaults.persistence.volumes.len(), 1);
    let vol = &defaults.persistence.volumes[0];
    assert_eq!(vol.name, "host-keys");
    assert_eq!(vol.mount_path, "/etc/ssh/keys");
    assert_eq!(vol.size, "10Mi");
    assert_eq!(vol.access_mode, "ReadWriteOnce");
}

// ---------------------------------------------------------------------------
// resolve_persistence: host-keys must survive a persistence override (#305)
// ---------------------------------------------------------------------------

#[test]
fn resolve_persistence_keeps_host_keys_with_no_override() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    let app = make_app(AppType::SshBastion);

    let persistence = defaults.resolve_persistence(&app).unwrap();

    assert_eq!(persistence.volumes.len(), 1);
    assert_eq!(persistence.volumes[0].name, "host-keys");
    assert_eq!(persistence.volumes[0].mount_path, "/etc/ssh/keys");
}

/// An override that explicitly names `host-keys` (e.g. to change its
/// storage class or size) must win over the compiled default.
#[test]
fn resolve_persistence_respects_explicit_host_keys_override() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    let mut app = make_app(AppType::SshBastion);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![PvcVolume {
            name: "host-keys".into(),
            mount_path: "/etc/ssh/keys".into(),
            access_mode: "ReadWriteOnce".into(),
            size: "50Mi".into(),
            storage_class: "custom-class".into(),
            existing_claim_name: None,
        }],
        nfs_mounts: vec![],
        ..Default::default()
    });

    let persistence = defaults.resolve_persistence(&app).unwrap();

    assert_eq!(persistence.volumes.len(), 1);
    assert_eq!(persistence.volumes[0].size, "50Mi");
    assert_eq!(persistence.volumes[0].storage_class, "custom-class");
}

/// Minimal `PvcVolume` for override volumes in tests where only the name and
/// mount_path matter.
fn vol(name: &str, mount_path: &str) -> PvcVolume {
    PvcVolume {
        name: name.into(),
        mount_path: mount_path.into(),
        access_mode: "ReadWriteOnce".into(),
        size: "1Gi".into(),
        storage_class: String::new(),
        existing_claim_name: None,
    }
}

/// Volume restoration (#305, generalized beyond SshBastion by #367) applies
/// to every app type's compiled default volumes: an unrelated persistence
/// override must not silently drop them.
#[test]
fn resolve_persistence_restores_dropped_default_volume() {
    let cases: &[(AppType, &str, &str)] = &[
        (AppType::SshBastion, "host-keys", "/etc/ssh/keys"),
        (AppType::Sonarr, "config", "/config"),
        (AppType::Sonarr, "downloads", "/downloads"),
        (AppType::Subgen, "models", "/subgen/models"),
        // Maintainerr's config volume is relocated to /opt/data (#131); the
        // restored volume must carry that relocation, not the generic /config.
        (AppType::Maintainerr, "config", "/opt/data"),
    ];

    for (app_type, expected_name, expected_mount_path) in cases.iter().cloned() {
        let defaults = AppDefaults::try_for_app(&app_type).unwrap();
        let mut app = make_app(app_type.clone());
        app.spec.persistence = Some(PersistenceSpec {
            volumes: vec![vol("unrelated", "/unrelated")],
            nfs_mounts: vec![],
            ..Default::default()
        });

        let persistence = defaults
            .resolve_persistence(&app)
            .unwrap_or_else(|e| panic!("resolve_persistence failed for {app_type:?}: {e}"));

        assert!(
            persistence.volumes.iter().any(|v| v.name == "unrelated"),
            "user-specified volume must survive for {app_type:?}"
        );
        let restored = persistence
            .volumes
            .iter()
            .find(|v| v.name == expected_name)
            .unwrap_or_else(|| {
                panic!(
                    "{expected_name} must not be dropped by an unrelated override for {app_type:?}"
                )
            });
        assert_eq!(
            restored.mount_path, expected_mount_path,
            "wrong mount_path for {expected_name} on {app_type:?}"
        );
    }
}

/// `PersistenceSpec::merge_with` carries the override's own tombstones
/// through even when the base has none.
#[test]
fn persistence_merge_with_carries_removed_default_volumes() {
    let override_spec = PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![],
        removed_default_volumes: vec!["downloads".into()],
    };
    let base = PersistenceSpec {
        volumes: vec![PvcVolume {
            name: "downloads".into(),
            mount_path: "/downloads".into(),
            access_mode: "ReadWriteOnce".into(),
            size: "100Gi".into(),
            storage_class: String::new(),
            existing_claim_name: None,
        }],
        nfs_mounts: vec![],
        removed_default_volumes: vec![],
    };

    let merged = override_spec.merge_with(&base);

    assert_eq!(
        merged.removed_default_volumes,
        vec!["downloads".to_string()]
    );
}

/// A base-layer tombstone (e.g. a MediaStack's `spec.defaults.persistence`)
/// is a removal policy, not an overridable value — it must survive a member
/// app that sets its own persistence override, or the tombstoned volume
/// silently comes back and collides (#386).
#[test]
fn persistence_merge_with_unions_removed_default_volumes_from_base() {
    let override_spec = PersistenceSpec {
        volumes: vec![PvcVolume {
            name: "media".into(),
            mount_path: "/media".into(),
            access_mode: "ReadWriteOnce".into(),
            size: "50Gi".into(),
            storage_class: String::new(),
            existing_claim_name: None,
        }],
        nfs_mounts: vec![],
        removed_default_volumes: vec!["config".into()],
    };
    let base = PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![],
        removed_default_volumes: vec!["downloads".into()],
    };

    let merged = override_spec.merge_with(&base);

    assert_eq!(merged.removed_default_volumes.len(), 2);
    assert!(
        merged
            .removed_default_volumes
            .contains(&"downloads".to_string())
    );
    assert!(
        merged
            .removed_default_volumes
            .contains(&"config".to_string())
    );
}

#[test]
fn ssh_bastion_has_no_nfs_mounts() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert!(defaults.persistence.nfs_mounts.is_empty());
}

#[test]
fn ssh_bastion_resources() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert_eq!(defaults.resources.limits.cpu, "500m");
    assert_eq!(defaults.resources.limits.memory, "256Mi");
    assert_eq!(defaults.resources.requests.cpu, "100m");
    assert_eq!(defaults.resources.requests.memory, "128Mi");
}

#[test]
fn ssh_bastion_has_tz_env() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert_eq!(defaults.env.len(), 1);
    assert_eq!(defaults.env[0].name, "TZ");
    assert_eq!(defaults.env[0].value, "UTC");
}

#[test]
fn ssh_bastion_has_no_app_config() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert!(defaults.app_config.is_none());
}

// ---------------------------------------------------------------------------
// TCP probe configuration (used by SSH bastion and tcp-probe-type apps)
// ---------------------------------------------------------------------------

#[test]
fn ssh_bastion_uses_tcp_probes() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();

    assert!(matches!(
        defaults.probes.liveness.probe_type,
        ProbeType::Tcp
    ));
    assert!(matches!(
        defaults.probes.readiness.probe_type,
        ProbeType::Tcp
    ));
}

#[test]
fn tcp_probe_liveness_parameters() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    let liveness = &defaults.probes.liveness;

    assert_eq!(liveness.initial_delay_seconds, 30);
    assert_eq!(liveness.period_seconds, 10);
    // #173: 5s timeout / 5 failures gives .NET *arr apps room for GC pauses
    // without silently disabling liveness detection.
    assert_eq!(liveness.timeout_seconds, 5);
    assert_eq!(liveness.failure_threshold, 5);
    // TCP probes inherit the default path from ProbeConfig::default() but it is
    // unused at runtime -- the operator ignores `path` for Tcp probe types.
}

#[test]
fn tcp_probe_readiness_parameters() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    let readiness = &defaults.probes.readiness;

    assert_eq!(readiness.initial_delay_seconds, 10);
    assert_eq!(readiness.period_seconds, 5);
    assert_eq!(readiness.timeout_seconds, 5);
    assert_eq!(readiness.failure_threshold, 5);
}

#[test]
fn tcp_probes_have_empty_command() {
    let defaults = AppDefaults::try_for_app(&AppType::SshBastion).unwrap();
    assert!(defaults.probes.liveness.command.is_empty());
    assert!(defaults.probes.readiness.command.is_empty());
}

// ---------------------------------------------------------------------------
// HTTP probe apps for comparison (ensure they are NOT tcp)
// ---------------------------------------------------------------------------

#[test]
fn http_apps_use_http_probes_not_tcp() {
    let http_apps = vec![
        AppType::Sonarr,
        AppType::Radarr,
        AppType::Lidarr,
        AppType::Prowlarr,
    ];

    for app_type in &http_apps {
        let defaults = AppDefaults::try_for_app(app_type).unwrap();
        assert!(
            matches!(defaults.probes.liveness.probe_type, ProbeType::Http),
            "{app_type} should use Http liveness probe"
        );
        assert!(
            matches!(defaults.probes.readiness.probe_type, ProbeType::Http),
            "{app_type} should use Http readiness probe"
        );
        assert!(
            !defaults.probes.liveness.path.is_empty(),
            "{app_type} should have a probe path"
        );
    }
}

#[test]
fn http_apps_liveness_timeout_tolerates_dotnet_gc_pauses() {
    // #173: Sonarr/Radarr/Lidarr are .NET apps whose HTTP server briefly stalls
    // during RSS syncs, library scans, and GC pauses. A 1s timeout / 3-failure
    // threshold (30s grace) was tight enough that these normal stalls triggered
    // SIGKILL restarts (68 restarts/4 days observed on Sonarr, all exit 137).
    let dotnet_apps = vec![AppType::Sonarr, AppType::Radarr, AppType::Lidarr];

    for app_type in &dotnet_apps {
        let defaults = AppDefaults::try_for_app(app_type).unwrap();
        assert_eq!(
            defaults.probes.liveness.timeout_seconds, 5,
            "{app_type} liveness timeout should tolerate brief GC-pause stalls"
        );
        assert_eq!(
            defaults.probes.liveness.failure_threshold, 5,
            "{app_type} liveness failure_threshold should give ~50s grace"
        );
    }
}

// ---------------------------------------------------------------------------
// SSH bastion tier and display
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// validate_all coverage (#610 follow-up)
// ---------------------------------------------------------------------------

/// `AppDefaults::validate_all()` is what gates operator startup against a broken
/// `image-defaults.toml` (see `controller::run`), but nothing previously asserted it actually
/// passes -- a missing/malformed entry for a new `AppType` variant would only surface at
/// startup in a real cluster, not in CI. This also backs the "provably unreachable" argument
/// for `servarr_resources::pvc::build_all`'s error branch (#610): since `validate_all` proves
/// every `AppType::ALL` variant resolves via `AppDefaults::try_for_app`, and `spec.app` is
/// always one of those variants (a closed enum, not an open string), `build_all` can never hit
/// its "no image defaults" / "unknown security profile" errors for any real `ServarrApp`.
#[test]
fn validate_all_passes_for_every_app_type() {
    assert!(
        AppDefaults::validate_all().is_ok(),
        "image-defaults.toml is missing or malformed for at least one AppType variant"
    );
}

#[test]
fn ssh_bastion_is_tier_zero() {
    assert_eq!(AppType::SshBastion.tier(), 0);
}

#[test]
fn ssh_bastion_display_name() {
    assert_eq!(AppType::SshBastion.to_string(), "ssh-bastion");
}

// ---------------------------------------------------------------------------
// ProbeConfig default values
// ---------------------------------------------------------------------------

#[test]
fn probe_config_default_is_http_with_standard_values() {
    let probe = ProbeConfig::default();
    assert!(matches!(probe.probe_type, ProbeType::Http));
    assert_eq!(probe.path, "/");
    assert!(probe.command.is_empty());
    assert_eq!(probe.initial_delay_seconds, 30);
    assert_eq!(probe.period_seconds, 10);
    assert_eq!(probe.timeout_seconds, 5);
    assert_eq!(probe.failure_threshold, 5);
}

// ---------------------------------------------------------------------------
// SecurityProfile::custom
// ---------------------------------------------------------------------------

#[test]
fn security_profile_custom_has_custom_type() {
    let profile = SecurityProfile::custom();
    assert!(matches!(profile.profile_type, SecurityProfileType::Custom));
}

// ---------------------------------------------------------------------------
// ProwlarrSyncSpec::default and SeerrSyncSpec::default
// ---------------------------------------------------------------------------

#[test]
fn prowlarr_sync_spec_default_values() {
    let spec = ProwlarrSyncSpec::default();
    assert!(!spec.enabled);
    assert!(spec.namespace_scope.is_none());
    assert!(spec.auto_remove);
}

#[test]
fn seerr_sync_spec_default_values() {
    let spec = SeerrSyncSpec::default();
    assert!(!spec.enabled);
    assert!(spec.namespace_scope.is_none());
    assert!(spec.auto_remove);
}

// ---------------------------------------------------------------------------
// Bazarr and Subgen tier and display
// ---------------------------------------------------------------------------

#[test]
fn bazarr_has_correct_tier() {
    assert_eq!(AppType::Bazarr.tier(), 3);
}

#[test]
fn subgen_has_correct_tier() {
    // #10: Subgen depends on Jellyfin so it belongs in tier 3 (Ancillary), not tier 0
    assert_eq!(AppType::Subgen.tier(), 3);
}

#[test]
fn bazarr_as_str() {
    assert_eq!(AppType::Bazarr.as_str(), "bazarr");
}

#[test]
fn subgen_as_str() {
    assert_eq!(AppType::Subgen.as_str(), "subgen");
}

// ---------------------------------------------------------------------------
// BazarrSyncSpec and SubgenSyncSpec defaults
// ---------------------------------------------------------------------------

#[test]
fn bazarr_sync_spec_default_values() {
    let spec = BazarrSyncSpec::default();
    assert!(!spec.enabled);
    assert!(spec.namespace_scope.is_none());
    assert!(spec.auto_remove);
}

#[test]
fn subgen_sync_spec_default_values() {
    let spec = SubgenSyncSpec::default();
    assert!(!spec.enabled);
    assert!(spec.namespace_scope.is_none());
}

// ---------------------------------------------------------------------------
// Subgen AppDefaults
// ---------------------------------------------------------------------------

#[test]
fn subgen_has_models_pvc() {
    let defaults = AppDefaults::try_for_app(&AppType::Subgen).unwrap();
    let has_models = defaults
        .persistence
        .volumes
        .iter()
        .any(|v| v.name == "models" && v.mount_path == "/subgen/models");
    assert!(
        has_models,
        "Subgen should have a 'models' PVC at /subgen/models"
    );
}

#[test]
fn subgen_default_env_includes_transcribe_device() {
    let defaults = AppDefaults::try_for_app(&AppType::Subgen).unwrap();
    let has_device = defaults
        .env
        .iter()
        .any(|e| e.name == "TRANSCRIBE_DEVICE" && e.value == "cpu");
    assert!(has_device, "Subgen should default TRANSCRIBE_DEVICE=cpu");
}

#[test]
fn subgen_default_env_includes_whisper_model() {
    let defaults = AppDefaults::try_for_app(&AppType::Subgen).unwrap();
    let has_model = defaults
        .env
        .iter()
        .any(|e| e.name == "WHISPER_MODEL" && e.value == "medium");
    assert!(has_model, "Subgen should default WHISPER_MODEL=medium");
}

#[test]
fn bazarr_defaults_are_linuxserver_profile() {
    let defaults = AppDefaults::try_for_app(&AppType::Bazarr).unwrap();
    // Bazarr uses linuxserver security profile — verify it builds without panicking
    // (build.rs codegen would have panicked at compile time if image-defaults.toml was
    // wrong)
    assert!(!defaults.persistence.volumes.is_empty());
}

// ---------------------------------------------------------------------------
// Download client memory defaults (issue #82)
// ---------------------------------------------------------------------------

fn assert_download_memory(app: &AppType) {
    let defaults = AppDefaults::try_for_app(app).unwrap();
    assert_eq!(defaults.resources.limits.memory, "1Gi");
    assert_eq!(defaults.resources.requests.memory, "256Mi");
}

#[test]
fn download_apps_get_higher_memory_default() {
    for app in [
        AppType::Sabnzbd,
        AppType::Transmission,
        AppType::Sonarr,
        AppType::Radarr,
        AppType::Lidarr,
    ] {
        assert_download_memory(&app);
    }
}

#[test]
fn non_download_apps_keep_lower_memory_default() {
    // Prowlarr is an indexer, not a download client
    let defaults = AppDefaults::try_for_app(&AppType::Prowlarr).unwrap();
    assert_eq!(defaults.resources.limits.memory, "512Mi");
    assert_eq!(defaults.resources.requests.memory, "128Mi");
}

// ---------------------------------------------------------------------------
// Maintainerr defaults (issue #131, #132, #138)
// ---------------------------------------------------------------------------

#[test]
fn maintainerr_config_volume_mount_path() {
    // Issue #131: Maintainerr v3 stores data at /opt/data, not /config
    let defaults =
        AppDefaults::try_for_app(&AppType::Maintainerr).expect("Maintainerr defaults should load");
    let config_vol = defaults
        .persistence
        .volumes
        .iter()
        .find(|v| v.name == "config")
        .expect("Maintainerr should have a config volume");
    assert_eq!(
        config_vol.mount_path, "/opt/data",
        "Maintainerr config should be mounted at /opt/data"
    );
}

#[test]
fn maintainerr_config_volume_mount_path_via_try_for_app() {
    // Verify both try_for_app and for_app apply the mount path fix
    let defaults = AppDefaults::try_for_app(&AppType::Maintainerr)
        .expect("Maintainerr try_for_app should load");
    let config_vol = defaults
        .persistence
        .volumes
        .iter()
        .find(|v| v.name == "config")
        .expect("Maintainerr should have a config volume");
    assert_eq!(
        config_vol.mount_path, "/opt/data",
        "Maintainerr config via try_for_app should be mounted at /opt/data"
    );
}

#[test]
fn maintainerr_has_higher_memory_for_large_scans() {
    // Issue #138: Maintainerr needs ≥1Gi for large library scans
    let defaults =
        AppDefaults::try_for_app(&AppType::Maintainerr).expect("Maintainerr defaults should load");
    assert_eq!(
        defaults.resources.limits.memory, "2Gi",
        "Maintainerr needs 2Gi memory limit for large library scans"
    );
    assert_eq!(
        defaults.resources.requests.memory, "512Mi",
        "Maintainerr should request 512Mi"
    );
}

#[test]
fn subgen_has_higher_memory_for_whisper_inference() {
    // Subgen uses Whisper medium model by default, needs ≥1.5Gi memory
    let defaults = AppDefaults::try_for_app(&AppType::Subgen).expect("Subgen defaults should load");
    assert_eq!(
        defaults.resources.limits.memory, "2Gi",
        "Subgen needs 2Gi memory limit for Whisper inference"
    );
    assert_eq!(
        defaults.resources.requests.memory, "512Mi",
        "Subgen should request 512Mi"
    );
}
/// An admin who explicitly tombstones a default volume via
/// `removedDefaultVolumes` must have it actually removed — not silently
/// restored by the very safety net #367 added (#376).
#[test]
fn resolve_persistence_honors_removed_default_volumes_tombstone() {
    let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
    let mut app = make_app(AppType::Sonarr);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![],
        removed_default_volumes: vec!["downloads".into()],
    });

    let persistence = defaults.resolve_persistence(&app).unwrap();

    assert!(
        !persistence.volumes.iter().any(|v| v.name == "downloads"),
        "tombstoned default volume must not be restored"
    );
    assert!(
        persistence.volumes.iter().any(|v| v.name == "config"),
        "non-tombstoned default volumes must still be restored"
    );
}

/// Two persistence entries claiming the same `mount_path` produce an invalid
/// pod spec downstream (two `volumeMounts` at one path) — this must fail the
/// reconcile loudly instead of silently reaching the API server (#376).
#[test]
fn resolve_persistence_errors_on_mount_path_collision() {
    let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
    let mut app = make_app(AppType::Sonarr);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![NfsMount {
            name: "downloads-nfs".into(),
            server: "nas.local".into(),
            path: "/export/downloads".into(),
            mount_path: "/downloads".into(),
            read_only: false,
        }],
        removed_default_volumes: vec![],
    });

    let err = defaults.resolve_persistence(&app).expect_err(
        "an NFS mount colliding with the still-restored 'downloads' default PVC must fail loudly",
    );

    assert!(
        err.contains("/downloads"),
        "error should name the colliding mount_path, got: {err}"
    );
}

/// Tombstoning the colliding default volume (rather than leaving it to
/// collide) is exactly how an admin is meant to resolve this (#376) — it
/// must not also trip the collision check.
#[test]
fn resolve_persistence_removed_default_volume_allows_nfs_mount_at_same_path() {
    let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
    let mut app = make_app(AppType::Sonarr);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![NfsMount {
            name: "downloads-nfs".into(),
            server: "nas.local".into(),
            path: "/export/downloads".into(),
            mount_path: "/downloads".into(),
            read_only: false,
        }],
        removed_default_volumes: vec!["downloads".into()],
    });

    let persistence = defaults.resolve_persistence(&app).expect(
        "tombstoning the colliding default volume must let the override's NFS mount through",
    );

    assert!(
        !persistence.volumes.iter().any(|v| v.name == "downloads"),
        "tombstoned default volume must not be restored"
    );
    assert!(
        persistence
            .nfs_mounts
            .iter()
            .any(|m| m.mount_path == "/downloads")
    );
    assert!(
        persistence.volumes.iter().any(|v| v.name == "config"),
        "other default volumes must still be restored"
    );
}

/// Every override-mount collision case #402 (and its follow-ups) covers:
/// trailing/doubled-slash normalization against a compiled default, `..`
/// traversal normalization (#465), symlink-alias normalization (#484), each
/// fixed operator-injected mount `build_volume_mounts` adds outside
/// `PersistenceSpec` (present only under the app type + config listed), and
/// outright rejection of a `..` segment that resolves to a non-reserved path
/// (#487). Negative cases confirm `/run/secrets/admin` is reserved only when
/// `adminCredentials` is actually set, and that a symlink alias only matches
/// at a full path-segment boundary (`/var/running` is not `/var/run`).
#[test]
fn resolve_persistence_mount_path_collisions() {
    struct Case {
        app_type: AppType,
        setup: fn(&mut ServarrApp),
        mount_path: &'static str,
        expect_err: bool,
        // `find_mount_path_collision` names the *reserved* entry's raw path in
        // the error, not the colliding override's — for a `..` traversal case
        // those differ, so the assertion needle must be overridable per case
        // instead of always deriving from `mount_path` (#465).
        expect_err_contains: Option<&'static str>,
    }
    fn no_setup(_app: &mut ServarrApp) {}
    fn with_admin_credentials(app: &mut ServarrApp) {
        app.spec.admin_credentials = Some(AdminCredentialsSpec {
            secret_name: "transmission-admin".into(),
        });
    }
    fn with_prowlarr_custom_definitions(app: &mut ServarrApp) {
        app.spec.app_config = Some(AppConfig::Prowlarr(ProwlarrConfig {
            custom_definitions: vec![IndexerDefinition {
                name: "my-tracker".into(),
                content: "---".into(),
            }],
        }));
    }
    fn with_ssh_bastion_authorized_key(app: &mut ServarrApp) {
        app.spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig {
            users: vec![SshUser {
                name: "alice".into(),
                uid: 1000,
                gid: 1000,
                public_keys: "ssh-ed25519 AAAA...".into(),
                ..Default::default()
            }],
            ..Default::default()
        }));
    }

    let cases = [
        Case {
            app_type: AppType::Sonarr,
            setup: no_setup,
            mount_path: "/downloads/",
            expect_err: true,
            expect_err_contains: None,
        },
        Case {
            app_type: AppType::Sonarr,
            setup: no_setup,
            mount_path: "/downloads//",
            expect_err: true,
            expect_err_contains: None,
        },
        Case {
            app_type: AppType::Transmission,
            setup: no_setup,
            mount_path: "/watch",
            expect_err: true,
            expect_err_contains: None,
        },
        Case {
            app_type: AppType::Transmission,
            setup: with_admin_credentials,
            mount_path: "/run/secrets/admin",
            expect_err: true,
            expect_err_contains: None,
        },
        Case {
            app_type: AppType::Transmission,
            setup: no_setup,
            mount_path: "/run/secrets/admin",
            expect_err: false,
            expect_err_contains: None,
        },
        Case {
            app_type: AppType::Transmission,
            setup: with_admin_credentials,
            mount_path: "/custom-cont-init.d/99-transmission-auth.sh",
            expect_err: true,
            expect_err_contains: None,
        },
        Case {
            app_type: AppType::Prowlarr,
            setup: with_prowlarr_custom_definitions,
            mount_path: "/config/Definitions/Custom",
            expect_err: true,
            expect_err_contains: None,
        },
        Case {
            app_type: AppType::SshBastion,
            setup: with_ssh_bastion_authorized_key,
            mount_path: "/etc/authorized_keys",
            expect_err: true,
            expect_err_contains: None,
        },
        Case {
            app_type: AppType::Transmission,
            setup: no_setup,
            mount_path: "/watch/foo/../../watch",
            expect_err: true,
            expect_err_contains: Some("reserved by the operator"),
        },
        Case {
            app_type: AppType::Sonarr,
            setup: no_setup,
            mount_path: "/downloads/../music",
            expect_err: true,
            expect_err_contains: Some("must not contain '..'"),
        },
        Case {
            app_type: AppType::Transmission,
            setup: with_admin_credentials,
            mount_path: "/var/run/secrets/admin",
            expect_err: true,
            expect_err_contains: Some("reserved by the operator"),
        },
        Case {
            app_type: AppType::Transmission,
            setup: no_setup,
            mount_path: "/var/running/leftover",
            expect_err: false,
            expect_err_contains: None,
        },
    ];

    for case in cases {
        let defaults = AppDefaults::try_for_app(&case.app_type).unwrap();
        let mut app = make_app(case.app_type.clone());
        (case.setup)(&mut app);
        app.spec.persistence = Some(PersistenceSpec {
            volumes: vec![],
            nfs_mounts: vec![NfsMount {
                name: "override-nfs".into(),
                server: "nas.local".into(),
                path: "/export".into(),
                mount_path: case.mount_path.into(),
                read_only: false,
            }],
            removed_default_volumes: vec![],
        });

        let result = defaults.resolve_persistence(&app);
        if case.expect_err {
            let err = match result {
                Err(e) => e,
                Ok(_) => panic!(
                    "expected an error for {:?} at '{}'",
                    case.app_type, case.mount_path
                ),
            };
            let needle = case
                .expect_err_contains
                .unwrap_or_else(|| case.mount_path.trim_end_matches('/'));
            assert!(
                err.contains(needle),
                "error should contain '{needle}' for mount_path '{}', got: {err}",
                case.mount_path
            );
        } else if let Err(e) = result {
            panic!(
                "expected no error for {:?} at '{}', got: {e}",
                case.app_type, case.mount_path
            );
        }
    }
}

/// A user-supplied volume *name* colliding with an operator-reserved name (not just a
/// reserved *path*) must also fail loudly — Kubernetes rejects two volumes sharing a name
/// the same way it rejects two mounts at one path (#485).
#[test]
fn resolve_persistence_errors_on_reserved_volume_name_collision() {
    let defaults = AppDefaults::try_for_app(&AppType::Transmission).unwrap();
    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "transmission-admin".into(),
    });
    // A non-colliding mount_path so only the volume *name* collides with the
    // operator-reserved "admin-credentials" volume (#465's admin-credentials mount).
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![vol("admin-credentials", "/unrelated")],
        nfs_mounts: vec![],
        removed_default_volumes: vec![],
    });

    let err = defaults
        .resolve_persistence(&app)
        .expect_err("a volume name matching an operator-reserved name must be rejected");
    assert!(
        err.contains("admin-credentials") && err.contains("reserved by the operator"),
        "error should name the reserved volume name, got: {err}"
    );
}

/// `operator_reserved_mounts` only enumerates fixed mount *paths* — several fixed pod-level
/// volume *names* `build_volumes` injects have no meaningful fixed path (a ConfigMap/Secret
/// mounted only inside an init container) or a per-user path with a fixed name, so they were
/// missing from volume-name collision detection entirely until `operator_reserved_volume_names`
/// was added (#485 follow-up, SEC-001).
type ReservedVolumeNameCase = (AppType, fn(&mut ServarrApp), &'static str);

#[test]
fn resolve_persistence_errors_on_reserved_volume_names_missing_fixed_path() {
    let cases: &[ReservedVolumeNameCase] = &[
        (AppType::Bazarr, |_app| {}, "bazarr-init-scripts"),
        (AppType::Bazarr, |_app| {}, "bazarr-api-key"),
        (
            AppType::Sabnzbd,
            |app| {
                app.spec.app_config = Some(AppConfig::Sabnzbd(SabnzbdConfig {
                    tar_unpack: true,
                    ..Default::default()
                }));
            },
            "tar-unpack-scripts",
        ),
        (
            AppType::Sabnzbd,
            |app| {
                app.spec.app_config = Some(AppConfig::Sabnzbd(SabnzbdConfig {
                    host_whitelist: vec!["sonarr.example.com".into()],
                    ..Default::default()
                }));
            },
            "sabnzbd-scripts",
        ),
        (
            AppType::SshBastion,
            |app| {
                app.spec.app_config = Some(AppConfig::SshBastion(SshBastionConfig {
                    users: vec![SshUser {
                        name: "alice".into(),
                        uid: 1000,
                        gid: 1000,
                        mode: SshMode::RestrictedRsync,
                        public_keys: "ssh-ed25519 AAAA".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }));
            },
            "restricted-rsync",
        ),
    ];

    for (app_type, setup, reserved_name) in cases {
        let defaults = AppDefaults::try_for_app(app_type).unwrap();
        let mut app = make_app(app_type.clone());
        setup(&mut app);
        app.spec.persistence = Some(PersistenceSpec {
            volumes: vec![vol(reserved_name, "/unrelated")],
            nfs_mounts: vec![],
            removed_default_volumes: vec![],
        });

        let err = defaults.resolve_persistence(&app).expect_err(&format!(
            "expected {app_type:?} volume name '{reserved_name}' to collide"
        ));
        assert!(
            err.contains(*reserved_name) && err.contains("reserved by the operator"),
            "error should name the reserved volume '{reserved_name}' for {app_type:?}, got: {err}"
        );
    }
}

/// An NFS mount's actual pod-spec volume name is prefixed `nfs-<name>` (see
/// `servarr_resources::deployment::build_volumes`) — a PVC volume named `nfs-data` and an
/// NFS mount named `data` produce the same pod-spec volume name even though their
/// `PersistenceSpec` names differ, so the name-collision check must compare under that
/// prefix, not the raw `PersistenceSpec` names (#485).
#[test]
fn resolve_persistence_errors_on_nfs_prefixed_volume_name_collision() {
    let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
    let mut app = make_app(AppType::Sonarr);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![vol("nfs-data", "/one")],
        nfs_mounts: vec![NfsMount {
            name: "data".into(),
            server: "nas.local".into(),
            path: "/export/data".into(),
            mount_path: "/two".into(),
            read_only: false,
        }],
        removed_default_volumes: vec![],
    });

    let err = defaults.resolve_persistence(&app).expect_err(
        "a PVC volume named 'nfs-data' and an NFS mount named 'data' must collide at the pod-spec volume name",
    );
    assert!(
        err.contains("nfs-data"),
        "error should name the colliding pod-spec volume name, got: {err}"
    );
}

/// A non-colliding volume name must still pass — the name-collision check must not be
/// over-eager and reject unrelated names.
#[test]
fn resolve_persistence_no_error_on_distinct_volume_names() {
    let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
    let mut app = make_app(AppType::Sonarr);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![vol("media", "/media")],
        nfs_mounts: vec![NfsMount {
            name: "backups".into(),
            server: "nas.local".into(),
            path: "/export/backups".into(),
            mount_path: "/backups".into(),
            read_only: false,
        }],
        removed_default_volumes: vec![],
    });

    defaults
        .resolve_persistence(&app)
        .expect("distinct volume names must not collide");
}

/// The reserved-mount collision message must name the operator's mount, not
/// misdirect the user to look for a "persistence entry" that isn't in their
/// spec (#402 follow-up).
#[test]
fn resolve_persistence_collision_message_names_operator_reserved_mount() {
    let defaults = AppDefaults::try_for_app(&AppType::Transmission).unwrap();
    let mut app = make_app(AppType::Transmission);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![NfsMount {
            name: "watch-nfs".into(),
            server: "nas.local".into(),
            path: "/export/watch".into(),
            mount_path: "/watch".into(),
            read_only: false,
        }],
        removed_default_volumes: vec![],
    });

    let err = defaults.resolve_persistence(&app).unwrap_err();
    assert!(
        err.contains("reserved by the operator"),
        "error should say the mount is operator-reserved, not imply another persistence entry, got: {err}"
    );
    assert!(
        err.contains("watch-nfs"),
        "error should name the user's own entry, got: {err}"
    );
}

/// The reserved-mount collision message must show the path the *user actually wrote*, not
/// whichever entry the internal iteration order happened to visit last — those differ once a
/// symlink-aliased override (#484) or a `..` traversal (#465) resolves to the same normalized
/// path as the reserved literal but isn't spelled the same way (SEC-006 follow-up).
#[test]
fn resolve_persistence_collision_message_names_users_own_raw_path() {
    let defaults = AppDefaults::try_for_app(&AppType::Transmission).unwrap();
    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "transmission-admin".into(),
    });
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![NfsMount {
            name: "admin-nfs".into(),
            server: "nas.local".into(),
            path: "/export/admin".into(),
            mount_path: "/var/run/secrets/admin".into(),
            read_only: false,
        }],
        removed_default_volumes: vec![],
    });

    let err = defaults.resolve_persistence(&app).unwrap_err();
    assert!(
        err.contains("/var/run/secrets/admin"),
        "error should show the path the user actually wrote, not the reserved literal \
         '/run/secrets/admin', got: {err}"
    );
}

#[test]
fn seerr_defaults_use_fixed_uid_gid_and_app_config_mount_path() {
    let defaults = AppDefaults::try_for_app(&AppType::Seerr).unwrap();
    assert_eq!(defaults.uid, 1000);
    assert_eq!(defaults.gid, 1000);
    assert_eq!(defaults.security.user, 1000);
    assert_eq!(defaults.security.group, 1000);
    let config_vol = defaults
        .persistence
        .volumes
        .iter()
        .find(|v| v.name == "config")
        .expect("seerr defaults must have a config volume");
    assert_eq!(config_vol.mount_path, "/app/config");
}

// ---------------------------------------------------------------------------
// Unpackerr, Cleanuparr, Houndarr — AppType basics (#604, #605, #606)
// ---------------------------------------------------------------------------

#[test]
fn unpackerr_as_str_and_tier() {
    assert_eq!(AppType::Unpackerr.as_str(), "unpackerr");
    assert_eq!(AppType::Unpackerr.tier(), 3);
}

#[test]
fn cleanuparr_as_str_and_tier() {
    assert_eq!(AppType::Cleanuparr.as_str(), "cleanuparr");
    assert_eq!(AppType::Cleanuparr.tier(), 3);
}

#[test]
fn houndarr_as_str_and_tier() {
    assert_eq!(AppType::Houndarr.as_str(), "houndarr");
    assert_eq!(AppType::Houndarr.tier(), 3);
}

#[test]
fn unpackerr_defaults_probe_port_5656() {
    let defaults = AppDefaults::try_for_app(&AppType::Unpackerr).unwrap();
    assert_eq!(defaults.service.ports.first().map(|p| p.port), Some(5656));
}

#[test]
fn cleanuparr_defaults_probe_port_11011() {
    let defaults = AppDefaults::try_for_app(&AppType::Cleanuparr).unwrap();
    assert_eq!(defaults.service.ports.first().map(|p| p.port), Some(11011));
}

#[test]
fn houndarr_defaults_probe_port_8877() {
    let defaults = AppDefaults::try_for_app(&AppType::Houndarr).unwrap();
    assert_eq!(defaults.service.ports.first().map(|p| p.port), Some(8877));
}

// ---------------------------------------------------------------------------
// CleanuparrSyncSpec, HoundarrSyncSpec (#605, #606)
// ---------------------------------------------------------------------------

#[test]
fn cleanuparr_sync_spec_defaults() {
    let spec: CleanuparrSyncSpec = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
    assert!(spec.enabled);
    assert!(spec.namespace_scope.is_none());
}

#[test]
fn houndarr_sync_spec_requires_admin_credentials_secret() {
    let spec: HoundarrSyncSpec =
        serde_json::from_str(r#"{"enabled":true,"adminCredentialsSecret":"houndarr-admin"}"#)
            .unwrap();
    assert!(spec.enabled);
    assert_eq!(spec.admin_credentials_secret, "houndarr-admin");
    assert!(spec.namespace_scope.is_none());
}
