use super::AppConfig;
use super::types::*;

include!(concat!(env!("OUT_DIR"), "/image_defaults.rs"));

#[derive(Clone, Debug)]
pub struct AppDefaults {
    pub image: ImageSpec,
    pub service: ServiceSpec,
    pub security: SecurityProfile,
    pub persistence: PersistenceSpec,
    pub probes: ProbeSpec,
    pub resources: ResourceRequirements,
    pub uid: i64,
    pub gid: i64,
    pub env: Vec<EnvVar>,
    pub app_config: Option<AppConfig>,
}

impl AppDefaults {
    /// Load defaults for `app`, returning an error if the app has no entry in
    /// `image-defaults.toml` or its security profile is unrecognised.
    ///
    /// Propagates the error to the caller rather than panicking. Call
    /// [`validate_all`] at startup to catch a broken `image-defaults.toml`
    /// before the first reconcile.
    ///
    /// # Errors
    ///
    /// Returns an error string if the app has no image defaults or an unknown
    /// security profile.
    ///
    /// # Error safety
    /// The returned `Err(String)` is always built from curated, internal-only data — the app
    /// name (a `ServarrApp.spec.app` enum variant) and static strings from this module. It never
    /// contains user-supplied secrets, upstream API response bodies, or raw
    /// `kube::Error`/`reqwest::Error` text, so callers may interpolate it directly into logs,
    /// Events, or status Conditions without going through a `log_summary()`-style reduction.
    pub fn try_for_app(app: &super::AppType) -> Result<Self, String> {
        let app_name = app.to_string();
        let img = image_defaults(&app_name)
            .ok_or_else(|| format!("no image defaults for app: {app_name}"))?;
        let mut defaults = match img.security {
            "linuxserver" => Self::linuxserver_base(img.port, img.downloads, img.probe_path),
            "nonroot" => Self::nonroot_base(img.port, img.downloads, img.probe_path),
            "sshd" => Self::sshd_base(img.port),
            other => {
                return Err(format!(
                    "unknown security profile in image-defaults.toml: {other}"
                ));
            }
        };
        if img.probe_type == "tcp" {
            defaults.probes = tcp_probes(30, 10);
        }
        defaults.image = image(img.repository, img.tag);
        if matches!(app, super::AppType::Transmission) {
            defaults.app_config =
                Some(AppConfig::Transmission(super::TransmissionConfig::default()));
        }
        if matches!(app, super::AppType::Subgen) {
            defaults
                .persistence
                .volumes
                .push(pvc("models", "/subgen/models", "10Gi"));
            defaults.env.extend([
                EnvVar {
                    name: "TRANSCRIBE_DEVICE".into(),
                    value: "cpu".into(),
                },
                EnvVar {
                    name: "WHISPER_MODEL".into(),
                    value: "medium".into(),
                },
            ]);
            // Whisper medium model requires ~1.5GB; 512Mi default causes OOM
            defaults.resources = elevated_workload_resources();
        }
        if matches!(app, super::AppType::Maintainerr) {
            // Issue #131: Maintainerr v3 expects /opt/data, not /config
            let config_vol = defaults
                .persistence
                .volumes
                .iter_mut()
                .find(|v| v.name == "config")
                .ok_or_else(|| "Maintainerr defaults must have a 'config' volume".to_string())?;
            config_vol.mount_path = "/opt/data".to_string();
            // Issue #138: Maintainerr needs higher memory for large library scans
            defaults.resources = elevated_workload_resources();
        }
        if matches!(app, super::AppType::Seerr) {
            // Issue #44: Seerr's image runs as a fixed `node` user (UID/GID 1000, not
            // configurable via PUID/PGID like the LinuxServer image it replaces), and
            // expects its config volume at /app/config, not /config.
            defaults.uid = 1000;
            defaults.gid = 1000;
            defaults.security = SecurityProfile::non_root(1000, 1000);
            let config_vol = defaults
                .persistence
                .volumes
                .iter_mut()
                .find(|v| v.name == "config")
                .ok_or_else(|| "Seerr defaults must have a 'config' volume".to_string())?;
            config_vol.mount_path = "/app/config".to_string();
        }
        Ok(defaults)
    }

    /// Validate that every known [`AppType`] has a complete entry in
    /// `image-defaults.toml`. Call this at operator startup so misconfiguration
    /// is caught before the first reconcile.
    ///
    /// # Errors
    ///
    /// Returns a combined error message listing every app with missing or
    /// invalid defaults.
    pub fn validate_all() -> Result<(), String> {
        use super::AppType;
        let errors: Vec<String> = AppType::ALL
            .iter()
            .filter_map(|app| Self::try_for_app(app).err())
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Merge `app`'s persistence override with these compiled defaults, then
    /// restore any default volume the merge dropped.
    ///
    /// `PersistenceSpec::merge_with` replaces `volumes` wholesale when the
    /// override is non-empty (e.g. a MediaStack's stack-wide persistence
    /// applied to a member with no per-app override). Every app-type default
    /// volume is load-bearing for that app — SshBastion's `host-keys` losing
    /// it regenerates the host's SSH identity and breaks every client's
    /// trust (#305); Subgen's `models`, the `downloads` volume, and
    /// Maintainerr's relocated `config` volume are equally load-bearing for
    /// their apps. Rather than special-case one app type, restore *any*
    /// compiled default volume the override's whole-list replace dropped. An
    /// explicit override that names a default volume itself still wins.
    pub fn resolve_persistence(&self, app: &super::ServarrApp) -> Result<PersistenceSpec, String> {
        let override_spec = app.spec.persistence.as_ref();

        let mut persistence = match override_spec {
            None => self.persistence.clone(),
            Some(spec) => spec.merge_with(&self.persistence),
        };

        // A tombstoned name is dropped unless the override itself re-lists
        // that volume explicitly — explicit still wins over "remove this".
        let tombstoned = override_spec
            .map(|spec| spec.removed_default_volumes.as_slice())
            .unwrap_or(&[]);
        let explicitly_kept = override_spec
            .map(|spec| spec.volumes.as_slice())
            .unwrap_or(&[]);
        let is_removed = |name: &str| {
            tombstoned.iter().any(|n| n == name) && !explicitly_kept.iter().any(|v| v.name == name)
        };

        for default_vol in &self.persistence.volumes {
            if !persistence
                .volumes
                .iter()
                .any(|v| v.name == default_vol.name)
            {
                persistence.volumes.push(default_vol.clone());
            }
        }
        persistence.volumes.retain(|v| !is_removed(&v.name));

        find_mount_path_collision(
            &persistence.volumes,
            &persistence.nfs_mounts,
            &operator_reserved_mounts(app),
        )?;

        Ok(persistence)
    }

    fn linuxserver_base(port: i32, downloads: bool, probe_path: &str) -> Self {
        let mut volumes = vec![pvc("config", "/config", "1Gi")];
        if downloads {
            volumes.push(pvc("downloads", "/downloads", "100Gi"));
        }
        let (mem_limit, mem_request) = if downloads {
            ("1Gi", "256Mi")
        } else {
            ("512Mi", "128Mi")
        };
        Self {
            image: ImageSpec::default(),
            service: single_port_service("http", port),
            security: SecurityProfile::linux_server(DEFAULT_UID, DEFAULT_GID),
            persistence: PersistenceSpec {
                volumes,
                nfs_mounts: vec![],
                ..Default::default()
            },
            probes: http_probes(probe_path, 30, 10),
            resources: std_resources("1", mem_limit, "100m", mem_request),
            uid: DEFAULT_UID,
            gid: DEFAULT_GID,
            env: vec![tz_env()],
            app_config: None,
        }
    }

    fn nonroot_base(port: i32, downloads: bool, probe_path: &str) -> Self {
        let mut volumes = vec![pvc("config", "/config", "1Gi")];
        if downloads {
            volumes.push(pvc("downloads", "/downloads", "100Gi"));
        }
        let (mem_limit, mem_request) = if downloads {
            ("1Gi", "256Mi")
        } else {
            ("512Mi", "128Mi")
        };
        Self {
            image: ImageSpec::default(),
            service: single_port_service("http", port),
            security: SecurityProfile::non_root(DEFAULT_UID, DEFAULT_GID),
            persistence: PersistenceSpec {
                volumes,
                nfs_mounts: vec![],
                ..Default::default()
            },
            probes: http_probes(probe_path, 30, 10),
            resources: std_resources("1", mem_limit, "100m", mem_request),
            uid: DEFAULT_UID,
            gid: DEFAULT_GID,
            env: vec![tz_env()],
            app_config: None,
        }
    }

    /// SSH bastion: needs CHOWN/SETGID/SETUID/NET_BIND_SERVICE/SYS_CHROOT,
    /// runs as root for user management, uses TCP probes on SSH port.
    fn sshd_base(port: i32) -> Self {
        Self {
            image: ImageSpec::default(),
            service: single_port_service("ssh", port),
            security: SecurityProfile {
                profile_type: SecurityProfileType::Custom,
                user: 0,
                group: 0,
                run_as_non_root: Some(false),
                read_only_root_filesystem: Some(false),
                allow_privilege_escalation: Some(false),
                capabilities_add: vec![
                    "CHOWN".into(),
                    "SETGID".into(),
                    "SETUID".into(),
                    "NET_BIND_SERVICE".into(),
                    "SYS_CHROOT".into(),
                ],
                capabilities_drop: vec!["ALL".into()],
            },
            persistence: PersistenceSpec {
                volumes: vec![pvc("host-keys", "/etc/ssh/keys", "10Mi")],
                nfs_mounts: vec![],
                ..Default::default()
            },
            probes: tcp_probes(30, 10),
            resources: std_resources("500m", "256Mi", "100m", "128Mi"),
            uid: 0,
            gid: 0,
            env: vec![tz_env()],
            app_config: None,
        }
    }
}

/// Mount paths the operator injects outside `PersistenceSpec` for certain app
/// types — see `servarr_resources::deployment::build_volume_mounts`, which
/// must inject exactly these paths (plus per-user ones this function
/// deliberately excludes, see below). A user's persistence override must not
/// collide with these either, even though they never appear in
/// `PersistenceSpec` (#402). Scoped to the fixed, non-per-user mounts a real
/// override could plausibly name; per-user paths (SSH bastion's
/// `/home/<user>/.ssh`, restricted-rsync scripts) are parameterized by user
/// name and not sensible collision targets for a persistence override.
///
/// `servarr-crds` has no compile-time link to `servarr-resources` (the
/// dependency runs the other way), so this list and `build_volume_mounts`'s
/// literals can drift silently if one is updated without the other (#408).
/// `servarr-resources`' `test_operator_reserved_mounts_matches_build_volume_mounts`
/// integration test is the drift guard — it fails if the two fall out of sync.
///
/// `pub` (rather than `pub(crate)`) only so that cross-crate test can call it;
/// not meant as stable public API for downstream consumers of this crate.
#[doc(hidden)]
pub fn operator_reserved_mounts(app: &super::ServarrApp) -> Vec<(&'static str, &'static str)> {
    let mut reserved = Vec::new();
    if matches!(app.spec.app, super::AppType::Transmission) {
        reserved.push(("/watch", "watch"));
        if app.spec.admin_credentials.is_some() {
            reserved.push(("/run/secrets/admin", "admin-credentials"));
            reserved.push(("/custom-cont-init.d/99-transmission-auth.sh", "scripts"));
        }
    }
    if let Some(super::AppConfig::Prowlarr(pc)) = &app.spec.app_config
        && !pc.custom_definitions.is_empty()
    {
        reserved.push(("/config/Definitions/Custom", "prowlarr-definitions"));
    }
    if let Some(super::AppConfig::SshBastion(sc)) = &app.spec.app_config
        && sc.users.iter().any(|u| !u.public_keys.is_empty())
    {
        reserved.push(("/etc/authorized_keys.src", "authorized-keys-src"));
        reserved.push(("/etc/authorized_keys", "authorized-keys"));
    }
    reserved
}

/// Kubernetes treats a trailing slash, a doubled slash, or a `.` segment as
/// part of the same path component sequence (`/downloads`, `/downloads/`, and
/// `/downloads//` all resolve to one mount point) — and a `..` segment
/// resolves the same way (`/watch/foo/../../watch` also resolves to
/// `/watch`) — so paths are normalized to their resolved component sequence
/// before compare. A flat filter that only drops empty and `.` segments
/// leaves `..` traversal able to dodge the reserved-mount collision check
/// it's meant to enforce (#465).
fn normalize_mount_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    format!("/{}", segments.join("/"))
}

/// Kubernetes rejects a pod spec with two `volumeMounts` at the same path —
/// this catches that at resolve time (across PVC volumes, NFS mounts, and
/// operator-injected mounts) so the reconcile fails loudly with a clear cause
/// instead of producing an invalid pod spec the API server silently rejects
/// (#376, #402).
fn find_mount_path_collision(
    volumes: &[PvcVolume],
    nfs_mounts: &[NfsMount],
    reserved: &[(&str, &str)],
) -> Result<(), String> {
    // (mount_path, name, is_reserved) — is_reserved picks the error wording
    // below, since a reserved name never appears in the user's own spec and
    // "persistence entry" would send them looking for it there (#402).
    let mut seen: std::collections::HashMap<String, (&str, bool)> =
        std::collections::HashMap::new();
    let entries = volumes
        .iter()
        .map(|v| (v.mount_path.as_str(), v.name.as_str(), false))
        .chain(
            nfs_mounts
                .iter()
                .map(|m| (m.mount_path.as_str(), m.name.as_str(), false)),
        )
        .chain(reserved.iter().map(|(path, name)| (*path, *name, true)));
    for (mount_path, name, is_reserved) in entries {
        let normalized = normalize_mount_path(mount_path);
        if let Some((prior, prior_reserved)) = seen.insert(normalized, (name, is_reserved)) {
            return Err(if prior_reserved || is_reserved {
                let user_entry = if prior_reserved { name } else { prior };
                format!(
                    "persistence entry '{user_entry}' mounts at '{mount_path}', which is reserved by the operator"
                )
            } else {
                format!("persistence entries '{prior}' and '{name}' both mount at '{mount_path}'")
            });
        }
    }
    Ok(())
}

fn image(repo: &str, tag: &str) -> ImageSpec {
    ImageSpec {
        repository: repo.into(),
        tag: tag.into(),
        digest: String::new(),
        pull_policy: "IfNotPresent".into(),
    }
}

fn pvc(name: &str, mount: &str, size: &str) -> PvcVolume {
    PvcVolume {
        name: name.into(),
        mount_path: mount.into(),
        access_mode: "ReadWriteOnce".into(),
        size: size.into(),
        storage_class: String::new(),
        existing_claim_name: None,
    }
}

fn sport(name: &str, port: i32) -> ServicePort {
    ServicePort {
        name: name.into(),
        port,
        protocol: "TCP".into(),
        container_port: None,
        host_port: None,
    }
}

fn single_port_service(name: &str, port: i32) -> ServiceSpec {
    ServiceSpec {
        service_type: "ClusterIP".into(),
        ports: vec![sport(name, port)],
    }
}

fn tcp_probes(liveness_delay: i32, readiness_delay: i32) -> ProbeSpec {
    ProbeSpec {
        liveness: ProbeConfig {
            probe_type: ProbeType::Tcp,
            initial_delay_seconds: liveness_delay,
            period_seconds: 10,
            ..Default::default()
        },
        readiness: ProbeConfig {
            probe_type: ProbeType::Tcp,
            initial_delay_seconds: readiness_delay,
            period_seconds: default_readiness_period(),
            ..Default::default()
        },
    }
}

fn http_probes(path: &str, liveness_delay: i32, readiness_delay: i32) -> ProbeSpec {
    ProbeSpec {
        liveness: ProbeConfig {
            probe_type: ProbeType::Http,
            path: path.into(),
            initial_delay_seconds: liveness_delay,
            period_seconds: 10,
            ..Default::default()
        },
        readiness: ProbeConfig {
            probe_type: ProbeType::Http,
            path: path.into(),
            initial_delay_seconds: readiness_delay,
            period_seconds: default_readiness_period(),
            ..Default::default()
        },
    }
}

fn std_resources(
    cpu_limit: &str,
    mem_limit: &str,
    cpu_req: &str,
    mem_req: &str,
) -> ResourceRequirements {
    ResourceRequirements {
        limits: ResourceList {
            cpu: cpu_limit.into(),
            memory: mem_limit.into(),
        },
        requests: ResourceList {
            cpu: cpu_req.into(),
            memory: mem_req.into(),
        },
    }
}

fn elevated_workload_resources() -> ResourceRequirements {
    std_resources("1", "2Gi", "100m", "512Mi")
}

fn tz_env() -> EnvVar {
    EnvVar {
        name: "TZ".into(),
        value: "UTC".into(),
    }
}
