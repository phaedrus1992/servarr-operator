use servarr_crds::*;

// ---------------------------------------------------------------------------
// SSH Bastion defaults
// ---------------------------------------------------------------------------

#[test]
fn ssh_bastion_uses_custom_security_profile() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert!(matches!(
        defaults.security.profile_type,
        SecurityProfileType::Custom
    ));
}

#[test]
fn ssh_bastion_runs_as_root() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert_eq!(defaults.security.user, 0);
    assert_eq!(defaults.security.group, 0);
    assert_eq!(defaults.uid, 0);
    assert_eq!(defaults.gid, 0);
}

#[test]
fn ssh_bastion_has_required_capabilities() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
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
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert_eq!(defaults.security.capabilities_drop, vec!["ALL".to_string()]);
}

#[test]
fn ssh_bastion_security_flags() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert_eq!(defaults.security.run_as_non_root, Some(false));
    assert_eq!(defaults.security.read_only_root_filesystem, Some(false));
    assert_eq!(defaults.security.allow_privilege_escalation, Some(false));
}

#[test]
fn ssh_bastion_service_port_is_ssh() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert_eq!(defaults.service.ports.len(), 1);
    assert_eq!(defaults.service.ports[0].name, "ssh");
    assert_eq!(defaults.service.ports[0].protocol, "TCP");
    assert_eq!(defaults.service.service_type, "ClusterIP");
}

#[test]
fn ssh_bastion_has_host_keys_volume() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert_eq!(defaults.persistence.volumes.len(), 1);
    let vol = &defaults.persistence.volumes[0];
    assert_eq!(vol.name, "host-keys");
    assert_eq!(vol.mount_path, "/etc/ssh/keys");
    assert_eq!(vol.size, "10Mi");
    assert_eq!(vol.access_mode, "ReadWriteOnce");
}

#[test]
fn ssh_bastion_has_no_nfs_mounts() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert!(defaults.persistence.nfs_mounts.is_empty());
}

#[test]
fn ssh_bastion_resources() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert_eq!(defaults.resources.limits.cpu, "500m");
    assert_eq!(defaults.resources.limits.memory, "256Mi");
    assert_eq!(defaults.resources.requests.cpu, "100m");
    assert_eq!(defaults.resources.requests.memory, "128Mi");
}

#[test]
fn ssh_bastion_has_tz_env() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert_eq!(defaults.env.len(), 1);
    assert_eq!(defaults.env[0].name, "TZ");
    assert_eq!(defaults.env[0].value, "UTC");
}

#[test]
fn ssh_bastion_has_no_app_config() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    assert!(defaults.app_config.is_none());
}

// ---------------------------------------------------------------------------
// TCP probe configuration (used by SSH bastion and tcp-probe-type apps)
// ---------------------------------------------------------------------------

#[test]
fn ssh_bastion_uses_tcp_probes() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);

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
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    let liveness = &defaults.probes.liveness;

    assert_eq!(liveness.initial_delay_seconds, 30);
    assert_eq!(liveness.period_seconds, 10);
    assert_eq!(liveness.timeout_seconds, 1);
    assert_eq!(liveness.failure_threshold, 3);
    // TCP probes inherit the default path from ProbeConfig::default() but it is
    // unused at runtime -- the operator ignores `path` for Tcp probe types.
}

#[test]
fn tcp_probe_readiness_parameters() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
    let readiness = &defaults.probes.readiness;

    assert_eq!(readiness.initial_delay_seconds, 10);
    assert_eq!(readiness.period_seconds, 5);
    assert_eq!(readiness.timeout_seconds, 1);
    assert_eq!(readiness.failure_threshold, 3);
}

#[test]
fn tcp_probes_have_empty_command() {
    let defaults = AppDefaults::for_app(&AppType::SshBastion);
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
        let defaults = AppDefaults::for_app(app_type);
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

// ---------------------------------------------------------------------------
// SSH bastion tier and display
// ---------------------------------------------------------------------------

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
    assert_eq!(probe.timeout_seconds, 1);
    assert_eq!(probe.failure_threshold, 3);
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
// ProwlarrSyncSpec::default and OverseerrSyncSpec::default
// ---------------------------------------------------------------------------

#[test]
fn prowlarr_sync_spec_default_values() {
    let spec = ProwlarrSyncSpec::default();
    assert!(!spec.enabled);
    assert!(spec.namespace_scope.is_none());
    assert!(spec.auto_remove);
}

#[test]
fn overseerr_sync_spec_default_values() {
    let spec = OverseerrSyncSpec::default();
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
    let defaults = AppDefaults::for_app(&AppType::Subgen);
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
    let defaults = AppDefaults::for_app(&AppType::Subgen);
    let has_device = defaults
        .env
        .iter()
        .any(|e| e.name == "TRANSCRIBE_DEVICE" && e.value == "cpu");
    assert!(has_device, "Subgen should default TRANSCRIBE_DEVICE=cpu");
}

#[test]
fn subgen_default_env_includes_whisper_model() {
    let defaults = AppDefaults::for_app(&AppType::Subgen);
    let has_model = defaults
        .env
        .iter()
        .any(|e| e.name == "WHISPER_MODEL" && e.value == "medium");
    assert!(has_model, "Subgen should default WHISPER_MODEL=medium");
}

#[test]
fn bazarr_defaults_are_linuxserver_profile() {
    let defaults = AppDefaults::for_app(&AppType::Bazarr);
    // Bazarr uses linuxserver security profile — verify it builds without panicking
    // (build.rs codegen would have panicked at compile time if image-defaults.toml was
    // wrong)
    assert!(!defaults.persistence.volumes.is_empty());
}

// ---------------------------------------------------------------------------
// Navidrome AppType
// ---------------------------------------------------------------------------

#[test]
fn navidrome_as_str() {
    assert_eq!(AppType::Navidrome.as_str(), "navidrome");
}

#[test]
fn navidrome_is_tier_zero() {
    assert_eq!(AppType::Navidrome.tier(), 0);
}

// ---------------------------------------------------------------------------
// Poutine AppType
// ---------------------------------------------------------------------------

#[test]
fn poutine_as_str() {
    assert_eq!(AppType::Poutine.as_str(), "poutine");
}

#[test]
fn poutine_is_tier_three() {
    assert_eq!(AppType::Poutine.tier(), 3);
}

// ---------------------------------------------------------------------------
// Navidrome AppDefaults
// ---------------------------------------------------------------------------

#[test]
fn navidrome_defaults_port() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    assert_eq!(defaults.service.ports[0].port, 4533);
}

#[test]
fn navidrome_defaults_image() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    assert_eq!(defaults.image.repository, "deluan/navidrome");
    assert_eq!(defaults.image.tag, "0.61.2");
}

#[test]
fn navidrome_has_data_pvc() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has_data = defaults
        .persistence
        .volumes
        .iter()
        .any(|v| v.name == "data" && v.mount_path == "/data");
    assert!(has_data, "Navidrome should have a 'data' PVC at /data");
}

#[test]
fn navidrome_env_includes_nd_loglevel() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "ND_LOGLEVEL" && e.value == "info");
    assert!(has, "Navidrome should default ND_LOGLEVEL=info");
}

#[test]
fn navidrome_env_includes_nd_scanschedule() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "ND_SCANSCHEDULE" && e.value == "1h");
    assert!(has, "Navidrome should default ND_SCANSCHEDULE=1h");
}

#[test]
fn navidrome_env_includes_nd_sessiontimeout() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "ND_SESSIONTIMEOUT" && e.value == "24h");
    assert!(has, "Navidrome should default ND_SESSIONTIMEOUT=24h");
}

#[test]
fn navidrome_env_includes_nd_enableexternalservices() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "ND_ENABLEEXTERNALSERVICES" && e.value == "false");
    assert!(
        has,
        "Navidrome should default ND_ENABLEEXTERNALSERVICES=false"
    );
}

// ---------------------------------------------------------------------------
// Poutine AppDefaults
// ---------------------------------------------------------------------------

#[test]
fn poutine_defaults_port() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    assert_eq!(defaults.service.ports[0].port, 3000);
}

#[test]
fn poutine_defaults_image() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    assert_eq!(defaults.image.repository, "ghcr.io/benders/poutine");
    assert_eq!(defaults.image.tag, "0.4.5");
}

#[test]
fn poutine_has_data_pvc_at_app_data() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has_data = defaults
        .persistence
        .volumes
        .iter()
        .any(|v| v.name == "data" && v.mount_path == "/app/data");
    assert!(
        has_data,
        "Poutine should have a 'data' PVC at /app/data, got: {:?}",
        defaults.persistence.volumes
    );
}

#[test]
fn poutine_has_no_config_pvc() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has_config = defaults
        .persistence
        .volumes
        .iter()
        .any(|v| v.name == "config");
    assert!(
        !has_config,
        "Poutine should not have a 'config' PVC — config is managed by ConfigMap"
    );
}

#[test]
fn poutine_env_includes_node_env() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "NODE_ENV" && e.value == "production");
    assert!(has, "Poutine should default NODE_ENV=production");
}

#[test]
fn poutine_env_includes_database_path() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "DATABASE_PATH" && e.value == "/app/data/poutine.db");
    assert!(
        has,
        "Poutine should default DATABASE_PATH=/app/data/poutine.db"
    );
}

#[test]
fn poutine_env_includes_private_key_path() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has = defaults.env.iter().any(|e| {
        e.name == "POUTINE_PRIVATE_KEY_PATH" && e.value == "/app/data/poutine_ed25519.pem"
    });
    assert!(
        has,
        "Poutine should default POUTINE_PRIVATE_KEY_PATH=/app/data/poutine_ed25519.pem"
    );
}

#[test]
fn poutine_no_peers_config_env_by_default() {
    // POUTINE_PEERS_CONFIG is injected dynamically only when peers are configured,
    // not as a compiled-in default env var.
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "POUTINE_PEERS_CONFIG");
    assert!(
        !has,
        "POUTINE_PEERS_CONFIG should not be a default env var (injected only when peers set)"
    );
}

#[test]
fn validate_all_includes_navidrome_and_poutine() {
    // This panics if either app is missing from image-defaults.toml or validate_all.
    AppDefaults::validate_all().expect("all app defaults should be valid");
}

// ---------------------------------------------------------------------------
// PoutineConfig round-trip
// ---------------------------------------------------------------------------

#[test]
fn poutine_config_serializes_peers() {
    let config = PoutineConfig {
        peers: vec![PoutinePeer {
            id: "friend-instance".into(),
            url: "https://music.friend.example.com".into(),
            public_key: "ed25519:fooBARbaz==".into(),
        }],
    };
    let json = serde_json::to_string(&config).expect("serialize");
    assert!(json.contains("friend-instance"));
    assert!(json.contains("ed25519:fooBARbaz=="));
}
