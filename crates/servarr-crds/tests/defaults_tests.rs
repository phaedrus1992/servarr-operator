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
