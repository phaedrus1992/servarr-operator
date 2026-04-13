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
    /// Use [`for_app`] in the hot reconcile path (it panics on bad data, which
    /// should only happen if `image-defaults.toml` is incomplete); call
    /// [`validate_all`] at startup to catch that early.
    ///
    /// # Errors
    ///
    /// Returns an error string if the app has no image defaults or an unknown
    /// security profile.
    pub fn try_for_app(app: &super::AppType) -> Result<Self, String> {
        let app_name = app.to_string();
        let img = image_defaults(&app_name)
            .ok_or_else(|| format!("no image defaults for app: {app_name}"))?;
        let mut defaults = match img.security {
            "linuxserver" => Self::linuxserver_base(img.port, img.downloads, img.probe_path),
            "nonroot" => Self::nonroot_base(img.port, img.probe_path),
            "sshd" => Self::sshd_base(img.port),
            other => {
                return Err(format!(
                    "unknown security profile in image-defaults.toml: {other}"
                ))
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
        let all = [
            AppType::Sonarr,
            AppType::Radarr,
            AppType::Lidarr,
            AppType::Prowlarr,
            AppType::Sabnzbd,
            AppType::Transmission,
            AppType::Tautulli,
            AppType::Overseerr,
            AppType::Maintainerr,
            AppType::Jackett,
            AppType::Jellyfin,
            AppType::Plex,
            AppType::SshBastion,
            AppType::Bazarr,
            AppType::Subgen,
        ];
        let errors: Vec<String> = all
            .iter()
            .filter_map(|app| Self::try_for_app(app).err())
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub fn for_app(app: &super::AppType) -> Self {
        let app_name = app.to_string();
        let img = image_defaults(&app_name)
            .unwrap_or_else(|| panic!("no image defaults for app: {app_name}"));

        let mut defaults = match img.security {
            "linuxserver" => Self::linuxserver_base(img.port, img.downloads, img.probe_path),
            "nonroot" => Self::nonroot_base(img.port, img.probe_path),
            "sshd" => Self::sshd_base(img.port),
            other => panic!("unknown security profile in image-defaults.toml: {other}"),
        };

        // Override probes for TCP probe type
        if img.probe_type == "tcp" {
            defaults.probes = tcp_probes(30, 10);
        }

        defaults.image = image(img.repository, img.tag);

        // App-specific config
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
        }

        defaults
    }

    fn linuxserver_base(port: i32, downloads: bool, probe_path: &str) -> Self {
        let mut volumes = vec![pvc("config", "/config", "1Gi")];
        if downloads {
            volumes.push(pvc("downloads", "/downloads", "100Gi"));
        }
        Self {
            image: ImageSpec::default(),
            service: single_port_service("http", port),
            security: SecurityProfile::linux_server(65534, 65534),
            persistence: PersistenceSpec {
                volumes,
                nfs_mounts: vec![],
            },
            probes: http_probes(probe_path, 30, 10),
            resources: std_resources("1", "512Mi", "100m", "128Mi"),
            uid: 65534,
            gid: 65534,
            env: vec![tz_env()],
            app_config: None,
        }
    }

    fn nonroot_base(port: i32, probe_path: &str) -> Self {
        Self {
            image: ImageSpec::default(),
            service: single_port_service("http", port),
            security: SecurityProfile::non_root(65534, 65534),
            persistence: PersistenceSpec {
                volumes: vec![pvc("config", "/config", "1Gi")],
                nfs_mounts: vec![],
            },
            probes: http_probes(probe_path, 30, 10),
            resources: std_resources("1", "512Mi", "100m", "128Mi"),
            uid: 65534,
            gid: 65534,
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
            timeout_seconds: 1,
            failure_threshold: 3,
            ..Default::default()
        },
        readiness: ProbeConfig {
            probe_type: ProbeType::Tcp,
            initial_delay_seconds: readiness_delay,
            period_seconds: 5,
            timeout_seconds: 1,
            failure_threshold: 3,
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
            timeout_seconds: 1,
            failure_threshold: 3,
            ..Default::default()
        },
        readiness: ProbeConfig {
            probe_type: ProbeType::Http,
            path: path.into(),
            initial_delay_seconds: readiness_delay,
            period_seconds: 5,
            timeout_seconds: 1,
            failure_threshold: 3,
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

fn tz_env() -> EnvVar {
    EnvVar {
        name: "TZ".into(),
        value: "UTC".into(),
    }
}
