use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use servarr_crds::*;

fn make_app(app_type: AppType) -> ServarrApp {
    ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-123".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: app_type,
            ..Default::default()
        },
        status: None,
    }
}

#[test]
fn test_deployment_builder_sonarr() {
    let app = make_app(AppType::Sonarr);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());

    assert_eq!(deploy.metadata.name.as_deref(), Some("test-app"));
    assert_eq!(deploy.metadata.namespace.as_deref(), Some("media"));

    let spec = deploy.spec.unwrap();
    assert_eq!(spec.replicas, Some(1));

    let pod_spec = spec.template.spec.unwrap();
    assert_eq!(pod_spec.containers.len(), 1);

    let container = &pod_spec.containers[0];
    assert_eq!(container.name, "sonarr");
    assert_eq!(
        container.image.as_deref(),
        Some("linuxserver/sonarr:4.0.16")
    );

    // Check PUID/PGID env vars for LinuxServer
    let env = container.env.as_ref().unwrap();
    assert!(env.iter().any(|e| e.name == "PUID"));
    assert!(env.iter().any(|e| e.name == "PGID"));
    assert!(env.iter().any(|e| e.name == "TZ"));

    // Check ports
    let ports = container.ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].container_port, 8989);

    // Check volume mounts (config + downloads)
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(
        mounts
            .iter()
            .any(|m| m.name == "config" && m.mount_path == "/config")
    );
    assert!(
        mounts
            .iter()
            .any(|m| m.name == "downloads" && m.mount_path == "/downloads")
    );

    // Check security context
    let sec = container.security_context.as_ref().unwrap();
    assert_eq!(sec.run_as_non_root, Some(false));
    assert_eq!(sec.allow_privilege_escalation, Some(false));

    // Check pod security
    let pod_sec = pod_spec.security_context.as_ref().unwrap();
    assert_eq!(pod_sec.fs_group, Some(65534));

    // No init containers for standard apps
    assert!(pod_spec.init_containers.is_none());
}

#[test]
fn test_deployment_builder_maintainerr_nonroot() {
    let app = make_app(AppType::Maintainerr);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());

    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];

    assert_eq!(
        container.image.as_deref(),
        Some("ghcr.io/jorenn92/maintainerr:2.19.0")
    );

    let sec = container.security_context.as_ref().unwrap();
    assert_eq!(sec.run_as_non_root, Some(true));
    assert_eq!(sec.run_as_user, Some(65534));

    // NonRoot apps don't get PUID/PGID
    let env = container.env.as_ref().unwrap();
    assert!(!env.iter().any(|e| e.name == "PUID"));
}

#[test]
fn test_deployment_builder_transmission() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("transmission".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-456".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Transmission,
            app_config: Some(AppConfig::Transmission(TransmissionConfig {
                peer_port: Some(PeerPortConfig {
                    port: 51413,
                    host_port: true,
                    ..Default::default()
                }),
                auth: Some(TransmissionAuth {
                    secret_name: "tx-auth".into(),
                }),
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];

    // Check peer ports added
    let ports = container.ports.as_ref().unwrap();
    assert!(ports.iter().any(|p| p.name.as_deref() == Some("peer-tcp")));
    assert!(ports.iter().any(|p| p.name.as_deref() == Some("peer-udp")));

    // Check auth env from secret
    let env = container.env.as_ref().unwrap();
    let user_env = env.iter().find(|e| e.name == "USER").unwrap();
    assert!(user_env.value_from.is_some());

    // Check watch volume mount
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(mounts.iter().any(|m| m.name == "watch"));

    // Check init container exists and runs as the app uid so it can read/write
    // settings.json after chown (DAC_OVERRIDE is dropped from capabilities).
    let init = pod_spec.init_containers.as_ref().unwrap();
    assert_eq!(init.len(), 1);
    assert_eq!(init[0].name, "apply-settings");
    let init_sec = init[0].security_context.as_ref().unwrap();
    assert_eq!(
        init_sec.run_as_user,
        Some(65534),
        "init container must run as app uid"
    );
    assert_eq!(
        init_sec.run_as_group,
        Some(65534),
        "init container must run as app gid"
    );

    // Check volumes include scripts ConfigMap
    let volumes = pod_spec.volumes.as_ref().unwrap();
    assert!(volumes.iter().any(|v| v.name == "scripts"));
    assert!(volumes.iter().any(|v| v.name == "watch"));
}

#[test]
fn test_service_builder() {
    let app = make_app(AppType::Radarr);
    let svc = servarr_resources::service::build(&app);

    assert_eq!(svc.metadata.name.as_deref(), Some("test-app"));
    assert_eq!(svc.metadata.namespace.as_deref(), Some("media"));

    let spec = svc.spec.unwrap();
    assert_eq!(spec.type_.as_deref(), Some("ClusterIP"));

    let ports = spec.ports.unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].port, 7878);
}

#[test]
fn test_pvc_builder() {
    let app = make_app(AppType::Sonarr);
    let pvcs = servarr_resources::pvc::build_all(&app);

    assert_eq!(pvcs.len(), 2); // config + downloads

    let config_pvc = pvcs
        .iter()
        .find(|p| p.metadata.name.as_deref() == Some("test-app-config"));
    assert!(config_pvc.is_some());

    let downloads_pvc = pvcs
        .iter()
        .find(|p| p.metadata.name.as_deref() == Some("test-app-downloads"));
    assert!(downloads_pvc.is_some());
}

#[test]
fn test_pvc_builder_config_only() {
    let app = make_app(AppType::Prowlarr);
    let pvcs = servarr_resources::pvc::build_all(&app);
    assert_eq!(pvcs.len(), 1);
    assert_eq!(pvcs[0].metadata.name.as_deref(), Some("test-app-config"));
}

#[test]
fn test_pvc_ssh_bastion_shell_mode_creates_home_pvcs() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-pvc".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![
                    SshUser {
                        name: "alice".into(),
                        uid: 1001,
                        gid: 1001,
                        mode: SshMode::Shell,
                        restricted_rsync: None,
                        shell: None,
                        public_keys: String::new(),
                    },
                    SshUser {
                        name: "bob".into(),
                        uid: 1002,
                        gid: 1002,
                        mode: SshMode::Shell,
                        restricted_rsync: None,
                        shell: None,
                        public_keys: String::new(),
                    },
                ],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let pvcs = servarr_resources::pvc::build_all(&app);

    // host-keys PVC from app defaults + one per user
    assert!(
        pvcs.iter()
            .any(|p| p.metadata.name.as_deref() == Some("bastion-ssh-home-alice"))
    );
    assert!(
        pvcs.iter()
            .any(|p| p.metadata.name.as_deref() == Some("bastion-ssh-home-bob"))
    );

    let alice_pvc = pvcs
        .iter()
        .find(|p| p.metadata.name.as_deref() == Some("bastion-ssh-home-alice"))
        .unwrap();
    let spec = alice_pvc.spec.as_ref().unwrap();
    assert_eq!(
        spec.access_modes.as_deref(),
        Some(&["ReadWriteOnce".to_string()][..])
    );
    let storage = spec.resources.as_ref().unwrap().requests.as_ref().unwrap()["storage"]
        .0
        .as_str();
    assert_eq!(storage, "10Mi");
}

#[test]
fn test_pvc_ssh_bastion_non_shell_mode_no_home_pvcs() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-pvc2".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "alice".into(),
                    uid: 1001,
                    gid: 1001,
                    mode: SshMode::Sftp,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: String::new(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let pvcs = servarr_resources::pvc::build_all(&app);
    assert!(
        !pvcs.iter().any(|p| p
            .metadata
            .name
            .as_deref()
            .unwrap_or("")
            .contains("ssh-home")),
        "Non-shell modes must not create ssh-home PVCs"
    );
}

#[test]
fn test_deployment_ssh_bastion_shell_mode_home_mounts() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-shell".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "alice".into(),
                    uid: 1001,
                    gid: 1001,
                    mode: SshMode::Shell,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];
    let mounts = container.volume_mounts.as_ref().unwrap();
    let volumes = pod_spec.volumes.as_ref().unwrap();

    assert!(
        mounts
            .iter()
            .any(|m| m.name == "ssh-home-alice" && m.mount_path == "/home/alice/.ssh"),
        "Shell mode must mount ssh-home-alice at /home/alice/.ssh"
    );
    assert!(
        volumes.iter().any(|v| v.name == "ssh-home-alice"),
        "Shell mode must have ssh-home-alice volume"
    );

    let init = pod_spec.init_containers.as_ref().unwrap();
    assert!(
        init.iter().any(|c| c.name == "setup-ssh-home"),
        "Shell mode must have setup-ssh-home init container"
    );
    let setup = init.iter().find(|c| c.name == "setup-ssh-home").unwrap();
    let setup_mounts = setup.volume_mounts.as_ref().unwrap();
    assert!(
        setup_mounts.iter().any(|m| m.name == "ssh-home-alice"),
        "setup-ssh-home must mount ssh-home-alice"
    );
}

#[test]
fn test_networkpolicy_builder() {
    let app = make_app(AppType::Sonarr);
    let np = servarr_resources::networkpolicy::build(&app);

    assert_eq!(np.metadata.name.as_deref(), Some("test-app"));
    let spec = np.spec.unwrap();
    let ingress = spec.ingress.unwrap();
    assert_eq!(ingress.len(), 1);
    let ports = ingress[0].ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
}

#[test]
fn test_configmap_builder_transmission() {
    let app = make_app(AppType::Transmission);
    let cm = servarr_resources::configmap::build(&app);
    assert!(cm.is_some());

    let cm = cm.unwrap();
    let data = cm.data.unwrap();
    assert!(data.contains_key("settings-override.json"));
    assert!(data.contains_key("apply-settings.sh"));

    let script = &data["apply-settings.sh"];
    assert!(script.contains("jq"));
    assert!(script.contains("chown"));
}

#[test]
fn test_configmap_builder_non_transmission() {
    let app = make_app(AppType::Sonarr);
    let cm = servarr_resources::configmap::build(&app);
    assert!(cm.is_none());
}

#[test]
fn test_httproute_builder_disabled() {
    let app = make_app(AppType::Sonarr);
    let route = servarr_resources::httproute::build(&app);
    assert!(route.is_none());
}

#[test]
fn test_httproute_builder_enabled() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-789".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                route_type: RouteType::Http,
                parent_refs: vec![GatewayParentRef {
                    name: "istio-gateway".into(),
                    namespace: "istio-system".into(),
                    section_name: String::new(),
                }],
                hosts: vec!["sonarr.example.com".into()],
                tls: None,
            }),
            ..Default::default()
        },
        status: None,
    };

    let route = servarr_resources::httproute::build(&app);
    assert!(route.is_some());
}

#[test]
fn test_custom_env_override() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-abc".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            env: vec![
                servarr_crds::EnvVar {
                    name: "TZ".into(),
                    value: "America/Chicago".into(),
                },
                servarr_crds::EnvVar {
                    name: "CUSTOM_VAR".into(),
                    value: "custom_value".into(),
                },
            ],
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    let env = container.env.as_ref().unwrap();

    // TZ should be overridden
    let tz = env.iter().find(|e| e.name == "TZ").unwrap();
    assert_eq!(tz.value.as_deref(), Some("America/Chicago"));

    // Custom var should be present
    assert!(env.iter().any(|e| e.name == "CUSTOM_VAR"));

    // Should not have duplicate TZ
    let tz_count = env.iter().filter(|e| e.name == "TZ").count();
    assert_eq!(tz_count, 1);
}

#[test]
fn test_custom_image_override() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-def".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            image: Some(ImageSpec {
                repository: "my-registry/sonarr".into(),
                tag: "custom".into(),
                digest: String::new(),
                pull_policy: "Always".into(),
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    assert_eq!(
        container.image.as_deref(),
        Some("my-registry/sonarr:custom")
    );
    assert_eq!(container.image_pull_policy.as_deref(), Some("Always"));
}

#[test]
fn test_image_digest_override() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-ghi".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            image: Some(ImageSpec {
                repository: "linuxserver/sonarr".into(),
                tag: "ignored".into(),
                digest: "sha256:abc123".into(),
                pull_policy: "IfNotPresent".into(),
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    assert_eq!(
        container.image.as_deref(),
        Some("linuxserver/sonarr@sha256:abc123")
    );
}

#[test]
fn test_nfs_mounts() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-jkl".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            persistence: Some(PersistenceSpec {
                volumes: vec![PvcVolume {
                    name: "config".into(),
                    mount_path: "/config".into(),
                    access_mode: "ReadWriteOnce".into(),
                    size: "1Gi".into(),
                    storage_class: String::new(),
                }],
                nfs_mounts: vec![NfsMount {
                    name: "media".into(),
                    server: "192.168.1.100".into(),
                    path: "/exports/media".into(),
                    mount_path: "/media".into(),
                    read_only: true,
                }],
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];

    let mounts = container.volume_mounts.as_ref().unwrap();
    let nfs_mount = mounts.iter().find(|m| m.name == "nfs-media").unwrap();
    assert_eq!(nfs_mount.mount_path, "/media");
    assert_eq!(nfs_mount.read_only, Some(true));

    let volumes = pod_spec.volumes.as_ref().unwrap();
    let nfs_vol = volumes.iter().find(|v| v.name == "nfs-media").unwrap();
    let nfs = nfs_vol.nfs.as_ref().unwrap();
    assert_eq!(nfs.server, "192.168.1.100");
    assert_eq!(nfs.path, "/exports/media");
}

#[test]
fn test_image_override_from_env() {
    let app = make_app(AppType::Sonarr);

    let mut overrides = std::collections::HashMap::new();
    overrides.insert(
        "sonarr".to_string(),
        ImageSpec {
            repository: "custom-registry/sonarr".into(),
            tag: "99.0.0".into(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        },
    );

    let deploy = servarr_resources::deployment::build(&app, &overrides);
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    assert_eq!(
        container.image.as_deref(),
        Some("custom-registry/sonarr:99.0.0")
    );
}

#[test]
fn test_deployment_builder_plex() {
    let app = make_app(AppType::Plex);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());

    let spec = deploy.spec.unwrap();
    let pod_spec = spec.template.spec.unwrap();
    let container = &pod_spec.containers[0];

    assert_eq!(container.name, "plex");
    assert_eq!(container.image.as_deref(), Some("linuxserver/plex:1.41.4"));

    // Check port
    let ports = container.ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].container_port, 32400);

    // LinuxServer security: PUID/PGID env vars
    let env = container.env.as_ref().unwrap();
    assert!(env.iter().any(|e| e.name == "PUID"));
    assert!(env.iter().any(|e| e.name == "PGID"));

    // Config-only: single volume mount
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(
        mounts
            .iter()
            .any(|m| m.name == "config" && m.mount_path == "/config")
    );
    assert!(
        !mounts.iter().any(|m| m.name == "downloads"),
        "Plex should not have a downloads volume"
    );

    // LinuxServer security context
    let sec = container.security_context.as_ref().unwrap();
    assert_eq!(sec.run_as_non_root, Some(false));
    assert_eq!(sec.allow_privilege_escalation, Some(false));
}

#[test]
fn test_deployment_builder_jellyfin() {
    let app = make_app(AppType::Jellyfin);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());

    let spec = deploy.spec.unwrap();
    let pod_spec = spec.template.spec.unwrap();
    let container = &pod_spec.containers[0];

    assert_eq!(container.name, "jellyfin");
    assert_eq!(
        container.image.as_deref(),
        Some("linuxserver/jellyfin:10.10.7")
    );

    // Check port
    let ports = container.ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].container_port, 8096);

    // LinuxServer security: PUID/PGID env vars
    let env = container.env.as_ref().unwrap();
    assert!(env.iter().any(|e| e.name == "PUID"));
    assert!(env.iter().any(|e| e.name == "PGID"));

    // Config-only: single volume mount
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(
        mounts
            .iter()
            .any(|m| m.name == "config" && m.mount_path == "/config")
    );
    assert!(
        !mounts.iter().any(|m| m.name == "downloads"),
        "Jellyfin should not have a downloads volume"
    );
}

#[test]
fn test_cr_image_overrides_env_override() {
    // CR-level image spec should take priority over env overrides
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-priority".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            image: Some(ImageSpec {
                repository: "cr-level/sonarr".into(),
                tag: "cr-tag".into(),
                digest: String::new(),
                pull_policy: "Always".into(),
            }),
            ..Default::default()
        },
        status: None,
    };

    let mut overrides = std::collections::HashMap::new();
    overrides.insert(
        "sonarr".to_string(),
        ImageSpec {
            repository: "env-level/sonarr".into(),
            tag: "env-tag".into(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        },
    );

    let deploy = servarr_resources::deployment::build(&app, &overrides);
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    // CR-level should win
    assert_eq!(container.image.as_deref(), Some("cr-level/sonarr:cr-tag"));
}

// ---------------------------------------------------------------------------
// secret::build_authorized_keys tests
// ---------------------------------------------------------------------------

#[test]
fn test_secret_non_ssh_app_returns_none() {
    let app = make_app(AppType::Sonarr);
    let secret = servarr_resources::secret::build_authorized_keys(&app);
    assert!(secret.is_none());
}

#[test]
fn test_secret_ssh_app_no_app_config_returns_none() {
    let app = make_app(AppType::SshBastion);
    let secret = servarr_resources::secret::build_authorized_keys(&app);
    assert!(secret.is_none());
}

#[test]
fn test_secret_ssh_app_empty_users_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("media".into()),
            uid: Some("uid-secret-1".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };
    let secret = servarr_resources::secret::build_authorized_keys(&app);
    assert!(secret.is_none());
}

#[test]
fn test_secret_ssh_app_users_with_empty_keys_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("media".into()),
            uid: Some("uid-secret-2".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "alice".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::Shell,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: String::new(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };
    let secret = servarr_resources::secret::build_authorized_keys(&app);
    assert!(secret.is_none());
}

#[test]
fn test_secret_ssh_app_valid_users_returns_secret() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("media".into()),
            uid: Some("uid-secret-3".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![
                    SshUser {
                        name: "alice".into(),
                        uid: 1000,
                        gid: 1000,
                        mode: SshMode::Shell,
                        restricted_rsync: None,
                        shell: None,
                        public_keys: "ssh-ed25519 AAAA alice@host".into(),
                    },
                    SshUser {
                        name: "bob".into(),
                        uid: 1001,
                        gid: 1001,
                        mode: SshMode::Shell,
                        restricted_rsync: None,
                        shell: None,
                        public_keys: "ssh-rsa BBBB bob@host".into(),
                    },
                ],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };
    let secret = servarr_resources::secret::build_authorized_keys(&app);
    assert!(secret.is_some());

    let secret = secret.unwrap();
    assert_eq!(
        secret.metadata.name.as_deref(),
        Some("bastion-authorized-keys")
    );
    assert_eq!(secret.metadata.namespace.as_deref(), Some("media"));
    assert_eq!(secret.type_.as_deref(), Some("Opaque"));

    let string_data = secret.string_data.unwrap();
    assert_eq!(string_data.len(), 2);
    assert_eq!(string_data["alice"], "ssh-ed25519 AAAA alice@host");
    assert_eq!(string_data["bob"], "ssh-rsa BBBB bob@host");

    // Owner references should be set
    let owner_refs = secret.metadata.owner_references.unwrap();
    assert_eq!(owner_refs.len(), 1);
    assert_eq!(owner_refs[0].uid, "uid-secret-3");
}

// ---------------------------------------------------------------------------
// certificate::build tests
// ---------------------------------------------------------------------------

#[test]
fn test_certificate_no_gateway_returns_none() {
    let app = make_app(AppType::Sonarr);
    let cert = servarr_resources::certificate::build(&app);
    assert!(cert.is_none());
}

#[test]
fn test_certificate_gateway_disabled_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-cert-1".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: false,
                ..Default::default()
            }),
            ..Default::default()
        },
        status: None,
    };
    let cert = servarr_resources::certificate::build(&app);
    assert!(cert.is_none());
}

#[test]
fn test_certificate_gateway_enabled_no_tls_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-cert-2".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                tls: None,
                ..Default::default()
            }),
            ..Default::default()
        },
        status: None,
    };
    let cert = servarr_resources::certificate::build(&app);
    assert!(cert.is_none());
}

#[test]
fn test_certificate_tls_disabled_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-cert-3".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                tls: Some(TlsSpec {
                    enabled: false,
                    cert_issuer: "letsencrypt".into(),
                    secret_name: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        status: None,
    };
    let cert = servarr_resources::certificate::build(&app);
    assert!(cert.is_none());
}

#[test]
fn test_certificate_tls_enabled_empty_issuer_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-cert-4".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                tls: Some(TlsSpec {
                    enabled: true,
                    cert_issuer: String::new(),
                    secret_name: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        status: None,
    };
    let cert = servarr_resources::certificate::build(&app);
    assert!(cert.is_none());
}

#[test]
fn test_certificate_tls_enabled_valid_issuer_returns_certificate() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-cert-5".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                hosts: vec!["sonarr.example.com".into(), "sonarr.local".into()],
                tls: Some(TlsSpec {
                    enabled: true,
                    cert_issuer: "letsencrypt-prod".into(),
                    secret_name: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        status: None,
    };
    let cert = servarr_resources::certificate::build(&app);
    assert!(cert.is_some());

    let cert = cert.unwrap();
    assert_eq!(cert.metadata.name.as_deref(), Some("test-app"));
    assert_eq!(cert.metadata.namespace.as_deref(), Some("media"));
    assert_eq!(cert.data["spec"]["secretName"], "test-app-tls");
    assert_eq!(cert.data["spec"]["issuerRef"]["name"], "letsencrypt-prod");
    assert_eq!(cert.data["spec"]["issuerRef"]["kind"], "ClusterIssuer");

    let dns_names = cert.data["spec"]["dnsNames"].as_array().unwrap();
    assert_eq!(dns_names.len(), 2);
    assert_eq!(dns_names[0], "sonarr.example.com");
    assert_eq!(dns_names[1], "sonarr.local");
}

#[test]
fn test_certificate_custom_secret_name() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-cert-6".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                hosts: vec!["sonarr.example.com".into()],
                tls: Some(TlsSpec {
                    enabled: true,
                    cert_issuer: "letsencrypt-prod".into(),
                    secret_name: Some("my-custom-tls-secret".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        status: None,
    };
    let cert = servarr_resources::certificate::build(&app).unwrap();
    assert_eq!(cert.metadata.name.as_deref(), Some("test-app"));
    assert_eq!(cert.data["spec"]["secretName"], "my-custom-tls-secret");
}

// ---------------------------------------------------------------------------
// tcproute::build tests
// ---------------------------------------------------------------------------

#[test]
fn test_tcproute_no_gateway_returns_none() {
    let app = make_app(AppType::Sonarr);
    let route = servarr_resources::tcproute::build(&app);
    assert!(route.is_none());
}

#[test]
fn test_tcproute_gateway_disabled_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-tcp-1".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: false,
                ..Default::default()
            }),
            ..Default::default()
        },
        status: None,
    };
    let route = servarr_resources::tcproute::build(&app);
    assert!(route.is_none());
}

#[test]
fn test_tcproute_http_route_no_tls_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-tcp-2".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                route_type: RouteType::Http,
                parent_refs: vec![GatewayParentRef {
                    name: "gw".into(),
                    namespace: String::new(),
                    section_name: String::new(),
                }],
                hosts: vec!["sonarr.example.com".into()],
                tls: None,
            }),
            ..Default::default()
        },
        status: None,
    };
    let route = servarr_resources::tcproute::build(&app);
    assert!(route.is_none());
}

#[test]
fn test_tcproute_tcp_route_type_returns_some() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-tcp-3".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                route_type: RouteType::Tcp,
                parent_refs: vec![GatewayParentRef {
                    name: "my-gateway".into(),
                    namespace: String::new(),
                    section_name: String::new(),
                }],
                hosts: vec![],
                tls: None,
            }),
            ..Default::default()
        },
        status: None,
    };
    let route = servarr_resources::tcproute::build(&app);
    assert!(route.is_some());

    let route = route.unwrap();
    assert_eq!(route.metadata.name.as_deref(), Some("test-app"));
    assert_eq!(route.metadata.namespace.as_deref(), Some("media"));

    // Check parent refs
    let parent_refs = route.data["spec"]["parentRefs"].as_array().unwrap();
    assert_eq!(parent_refs.len(), 1);
    assert_eq!(parent_refs[0]["name"], "my-gateway");
    // namespace is empty, should not be present
    assert!(parent_refs[0].get("namespace").is_none());

    // Check backend refs use default sonarr port (8989)
    let rules = route.data["spec"]["rules"].as_array().unwrap();
    let backend_refs = rules[0]["backendRefs"].as_array().unwrap();
    assert_eq!(backend_refs[0]["name"], "test-app");
    assert_eq!(backend_refs[0]["port"], 8989);
}

#[test]
fn test_tcproute_http_route_with_tls_enabled_returns_some() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-tcp-4".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                route_type: RouteType::Http,
                parent_refs: vec![GatewayParentRef {
                    name: "gw".into(),
                    namespace: "istio-system".into(),
                    section_name: String::new(),
                }],
                hosts: vec!["sonarr.example.com".into()],
                tls: Some(TlsSpec {
                    enabled: true,
                    cert_issuer: "letsencrypt".into(),
                    secret_name: None,
                }),
            }),
            ..Default::default()
        },
        status: None,
    };
    // TLS enabled forces TCP mode even when route_type is Http
    let route = servarr_resources::tcproute::build(&app);
    assert!(route.is_some());

    let route = route.unwrap();
    let parent_refs = route.data["spec"]["parentRefs"].as_array().unwrap();
    assert_eq!(parent_refs[0]["name"], "gw");
    assert_eq!(parent_refs[0]["namespace"], "istio-system");
}

#[test]
fn test_tcproute_parent_refs_with_namespace_and_section_name() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-tcp-5".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                route_type: RouteType::Tcp,
                parent_refs: vec![GatewayParentRef {
                    name: "my-gateway".into(),
                    namespace: "gateway-ns".into(),
                    section_name: "tcp-listener".into(),
                }],
                hosts: vec![],
                tls: None,
            }),
            ..Default::default()
        },
        status: None,
    };
    let route = servarr_resources::tcproute::build(&app).unwrap();

    let parent_refs = route.data["spec"]["parentRefs"].as_array().unwrap();
    assert_eq!(parent_refs.len(), 1);
    assert_eq!(parent_refs[0]["name"], "my-gateway");
    assert_eq!(parent_refs[0]["namespace"], "gateway-ns");
    assert_eq!(parent_refs[0]["sectionName"], "tcp-listener");
}

// ============================================================
// ConfigMap coverage tests
// ============================================================

#[test]
fn test_configmap_sabnzbd_with_host_whitelist() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sabnzbd".into()),
            namespace: Some("media".into()),
            uid: Some("uid-sab-wl".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sabnzbd,
            app_config: Some(AppConfig::Sabnzbd(SabnzbdConfig {
                host_whitelist: vec!["sabnzbd.example.com".into(), "sab.local".into()],
                tar_unpack: false,
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build(&app);
    assert!(
        cm.is_some(),
        "SABnzbd with host_whitelist should produce a ConfigMap"
    );
    let cm = cm.unwrap();
    let data = cm.data.unwrap();
    assert!(data.contains_key("apply-whitelist.sh"));
    assert!(data.contains_key("host-whitelist"));
    assert_eq!(data["host-whitelist"], "sabnzbd.example.com, sab.local");
}

#[test]
fn test_configmap_sabnzbd_empty_host_whitelist_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sabnzbd".into()),
            namespace: Some("media".into()),
            uid: Some("uid-sab-empty".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sabnzbd,
            app_config: Some(AppConfig::Sabnzbd(SabnzbdConfig {
                host_whitelist: vec![],
                tar_unpack: false,
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build(&app);
    assert!(
        cm.is_none(),
        "SABnzbd with empty host_whitelist should return None"
    );
}

#[test]
fn test_configmap_ssh_bastion_restricted_rsync() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-rr".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "backup".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::RestrictedRsync,
                    restricted_rsync: Some(RestrictedRsyncConfig {
                        allowed_paths: vec!["/data/backups".into(), "/media".into()],
                    }),
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_ssh_bastion_restricted_rsync(&app);
    assert!(
        cm.is_some(),
        "SSH bastion in RestrictedRsync mode should produce a ConfigMap"
    );
    let cm = cm.unwrap();
    let data = cm.data.unwrap();
    assert!(data.contains_key("restricted-rsync-backup.sh"));
    let script = &data["restricted-rsync-backup.sh"];
    assert!(script.contains("/data/backups"));
    assert!(script.contains("/media"));
    assert!(
        script.contains("--sender"),
        "script must enforce read-only via --sender check"
    );
    assert!(
        !script.contains("READONLY"),
        "READONLY variable must not exist; read-only is always enforced"
    );
}

#[test]
fn test_configmap_ssh_bastion_rsync_mode_produces_configmap() {
    // SshMode::Rsync should also produce the read-only rsync wrapper, without path restrictions.
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-rsync".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "backup".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::Rsync,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_ssh_bastion_restricted_rsync(&app);
    assert!(
        cm.is_some(),
        "SshMode::Rsync should produce a restricted-rsync ConfigMap"
    );
    let script = &cm.unwrap().data.unwrap()["restricted-rsync-backup.sh"];
    assert!(
        script.contains("--sender"),
        "script must enforce read-only via --sender check"
    );
    // No allowed paths configured — ALLOWED_PATHS array must be empty
    assert!(
        script.contains("ALLOWED_PATHS=(\n\n)") || script.contains("ALLOWED_PATHS=(\n)"),
        "Rsync mode must have empty ALLOWED_PATHS"
    );
}

#[test]
fn test_configmap_ssh_bastion_restricted_rsync_non_ssh_returns_none() {
    let app = make_app(AppType::Sonarr);
    let cm = servarr_resources::configmap::build_ssh_bastion_restricted_rsync(&app);
    assert!(
        cm.is_none(),
        "Non-SSH app should return None for restricted-rsync ConfigMap"
    );
}

#[test]
fn test_configmap_ssh_bastion_interactive_mode_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-int".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_ssh_bastion_restricted_rsync(&app);
    assert!(
        cm.is_none(),
        "SSH bastion with no rsync-mode users should return None for restricted-rsync"
    );
}

#[test]
fn test_configmap_prowlarr_definitions() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("prowlarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-prowl-def".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Prowlarr,
            app_config: Some(AppConfig::Prowlarr(ProwlarrConfig {
                custom_definitions: vec![
                    IndexerDefinition {
                        name: "my-tracker".into(),
                        content: "id: my-tracker\nname: My Tracker".into(),
                    },
                    IndexerDefinition {
                        name: "another-tracker".into(),
                        content: "id: another\nname: Another".into(),
                    },
                ],
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_prowlarr_definitions(&app);
    assert!(
        cm.is_some(),
        "Prowlarr with custom_definitions should produce a ConfigMap"
    );
    let cm = cm.unwrap();
    let data = cm.data.unwrap();
    assert!(data.contains_key("my-tracker.yml"));
    assert!(data.contains_key("another-tracker.yml"));
    assert_eq!(data["my-tracker.yml"], "id: my-tracker\nname: My Tracker");
}

#[test]
fn test_configmap_prowlarr_empty_definitions_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("prowlarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-prowl-empty".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Prowlarr,
            app_config: Some(AppConfig::Prowlarr(ProwlarrConfig {
                custom_definitions: vec![],
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_prowlarr_definitions(&app);
    assert!(
        cm.is_none(),
        "Prowlarr with empty custom_definitions should return None"
    );
}

#[test]
fn test_configmap_tar_unpack_enabled() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sabnzbd".into()),
            namespace: Some("media".into()),
            uid: Some("uid-sab-tar".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sabnzbd,
            app_config: Some(AppConfig::Sabnzbd(SabnzbdConfig {
                host_whitelist: vec![],
                tar_unpack: true,
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_tar_unpack(&app);
    assert!(
        cm.is_some(),
        "SABnzbd with tar_unpack=true should produce a ConfigMap"
    );
    let cm = cm.unwrap();
    let data = cm.data.unwrap();
    assert!(data.contains_key("install-tar-tools.sh"));
    assert!(data.contains_key("unpack-tar.sh"));
    assert!(data["install-tar-tools.sh"].contains("apk add"));
    assert!(data["unpack-tar.sh"].contains("tar"));
}

#[test]
fn test_configmap_tar_unpack_disabled_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sabnzbd".into()),
            namespace: Some("media".into()),
            uid: Some("uid-sab-notar".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sabnzbd,
            app_config: Some(AppConfig::Sabnzbd(SabnzbdConfig {
                host_whitelist: vec![],
                tar_unpack: false,
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_tar_unpack(&app);
    assert!(
        cm.is_none(),
        "SABnzbd with tar_unpack=false should return None"
    );
}

#[test]
fn test_configmap_transmission_custom_settings() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("transmission".into()),
            namespace: Some("media".into()),
            uid: Some("uid-tx-custom".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Transmission,
            app_config: Some(AppConfig::Transmission(TransmissionConfig {
                settings: serde_json::json!({
                    "download-dir": "/custom/downloads",
                    "speed-limit-up-enabled": true,
                    "speed-limit-up": 500
                }),
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build(&app);
    assert!(cm.is_some());
    let cm = cm.unwrap();
    let data = cm.data.unwrap();
    let settings = &data["settings-override.json"];
    assert!(settings.contains("/custom/downloads"));
    assert!(settings.contains("speed-limit-up"));
}

// ============================================================
// Deployment coverage tests
// ============================================================

#[test]
fn test_deployment_ssh_bastion_init_containers() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-deploy".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "backup".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::RestrictedRsync,
                    restricted_rsync: Some(RestrictedRsyncConfig {
                        allowed_paths: vec!["/data".into()],
                    }),
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    // Should have init containers: generate-host-keys and patch-entry
    let init = pod_spec.init_containers.as_ref().unwrap();
    assert!(
        init.iter().any(|c| c.name == "generate-host-keys"),
        "SSH bastion should have generate-host-keys init container"
    );
    assert!(
        init.iter().any(|c| c.name == "patch-entry"),
        "SSH bastion should have patch-entry init container"
    );

    // Should have authorized-keys directory mount (not per-user subPath).
    // panubo/sshd ≥1.10.0 exits with set -e when the read-only subPath file's
    // chmod returns 1, so we mount the whole Secret as a read-only directory so
    // entry.sh skips the chmod block.
    let container = &pod_spec.containers[0];
    let mounts = container.volume_mounts.as_ref().unwrap();
    let ak_mount = mounts.iter().find(|m| m.name == "authorized-keys");
    assert!(
        ak_mount.is_some(),
        "SSH bastion should have authorized-keys directory volume mount"
    );
    let ak_mount = ak_mount.unwrap();
    assert_eq!(
        ak_mount.mount_path, "/etc/authorized_keys",
        "authorized-keys should mount at /etc/authorized_keys"
    );
    assert_eq!(
        ak_mount.read_only,
        Some(true),
        "authorized-keys mount must be read-only"
    );
    assert!(
        ak_mount.sub_path.is_none(),
        "authorized-keys must be a directory mount, not a subPath file"
    );

    // Should have restricted-rsync volume mount
    assert!(
        mounts.iter().any(|m| m.name == "restricted-rsync"),
        "SSH bastion in RestrictedRsync mode should have restricted-rsync volume mount"
    );

    // Volumes should include authorized-keys secret and restricted-rsync configmap
    let volumes = pod_spec.volumes.as_ref().unwrap();
    assert!(
        volumes.iter().any(|v| v.name == "authorized-keys"),
        "Should have authorized-keys volume"
    );
    assert!(
        volumes.iter().any(|v| v.name == "restricted-rsync"),
        "Should have restricted-rsync volume"
    );

    // args should include -p with the SSH port so sshd listens on the right port
    let args = container
        .args
        .as_ref()
        .expect("SSH bastion should have args");
    let p_idx = args
        .iter()
        .position(|a| a == "-p")
        .expect("args should include -p flag");
    assert!(
        p_idx + 1 < args.len(),
        "args should have a port value after -p"
    );
}

#[test]
fn test_deployment_ssh_bastion_rsync_mode_uses_restricted_rsync() {
    // SshMode::Rsync must mount the restricted-rsync script and use it as the user shell.
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-rsync-deploy".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "backup".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::Rsync,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];
    let mounts = container.volume_mounts.as_ref().unwrap();

    assert!(
        mounts.iter().any(|m| m.name == "restricted-rsync"),
        "SshMode::Rsync must mount the restricted-rsync script"
    );

    let volumes = pod_spec.volumes.as_ref().unwrap();
    assert!(
        volumes.iter().any(|v| v.name == "restricted-rsync"),
        "SshMode::Rsync must have restricted-rsync volume"
    );

    // Shell must be the restricted-rsync wrapper
    let env = container.env.as_ref().unwrap();
    let ssh_users = env.iter().find(|e| e.name == "SSH_USERS").unwrap();
    assert!(
        ssh_users
            .value
            .as_deref()
            .unwrap_or("")
            .contains("/usr/local/bin/restricted-rsync-backup"),
        "SshMode::Rsync user shell must be /usr/local/bin/restricted-rsync-backup"
    );
}

#[test]
fn test_deployment_ssh_bastion_shell_package_installed() {
    // A user with /bin/bash should trigger apk add bash in the patch-entry script.
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-shell-pkg".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "alice".into(),
                    uid: 1001,
                    gid: 1001,
                    mode: SshMode::Shell,
                    restricted_rsync: None,
                    shell: Some("/bin/bash".into()),
                    public_keys: String::new(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let init = pod_spec.init_containers.as_ref().unwrap();
    let patch = init.iter().find(|c| c.name == "patch-entry").unwrap();
    let script = patch.command.as_ref().unwrap().last().unwrap();
    assert!(
        script.contains("apk add"),
        "patch-entry should install packages"
    );
    assert!(
        script.contains("bash"),
        "patch-entry should install bash for /bin/bash users"
    );
}

#[test]
fn test_deployment_ssh_bastion_default_shell_no_install() {
    // Users with no shell (defaults to /bin/sh) must NOT trigger apk installs.
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-sh-pkg".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "bob".into(),
                    uid: 1002,
                    gid: 1002,
                    mode: SshMode::Shell,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: String::new(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let init = pod_spec.init_containers.as_ref().unwrap();
    let patch = init.iter().find(|c| c.name == "patch-entry").unwrap();
    let script = patch.command.as_ref().unwrap().last().unwrap();
    assert!(
        !script.contains("apk add --no-cache bash")
            && !script.contains("apk add --no-cache zsh")
            && !script.contains("apk add --no-cache fish"),
        "patch-entry must not install shell packages when all users use /bin/sh"
    );
}

#[test]
fn test_deployment_sabnzbd_host_whitelist_init_container() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sabnzbd".into()),
            namespace: Some("media".into()),
            uid: Some("uid-sab-wl-deploy".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sabnzbd,
            app_config: Some(AppConfig::Sabnzbd(SabnzbdConfig {
                host_whitelist: vec!["sabnzbd.example.com".into()],
                tar_unpack: false,
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    let init = pod_spec.init_containers.as_ref().unwrap();
    assert!(
        init.iter().any(|c| c.name == "apply-whitelist"),
        "SABnzbd with host_whitelist should have apply-whitelist init container"
    );

    // Verify the whitelist CSV is passed as an argument
    let whitelist_init = init.iter().find(|c| c.name == "apply-whitelist").unwrap();
    let cmd = whitelist_init.command.as_ref().unwrap();
    assert!(
        cmd.iter().any(|arg| arg.contains("sabnzbd.example.com")),
        "Init container command should contain the whitelist CSV"
    );
}

#[test]
fn test_deployment_sabnzbd_tar_unpack_init_containers() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sabnzbd".into()),
            namespace: Some("media".into()),
            uid: Some("uid-sab-tar-deploy".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sabnzbd,
            app_config: Some(AppConfig::Sabnzbd(SabnzbdConfig {
                host_whitelist: vec![],
                tar_unpack: true,
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    let init = pod_spec.init_containers.as_ref().unwrap();
    assert!(
        init.iter().any(|c| c.name == "install-tar-tools"),
        "SABnzbd with tar_unpack should have install-tar-tools init container"
    );

    // Check that tar-unpack-scripts volume exists
    let volumes = pod_spec.volumes.as_ref().unwrap();
    assert!(
        volumes.iter().any(|v| v.name == "tar-unpack-scripts"),
        "Should have tar-unpack-scripts volume"
    );
}

#[test]
fn test_deployment_prowlarr_definitions_volume() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("prowlarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-prowl-deploy".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Prowlarr,
            app_config: Some(AppConfig::Prowlarr(ProwlarrConfig {
                custom_definitions: vec![IndexerDefinition {
                    name: "my-tracker".into(),
                    content: "id: my-tracker".into(),
                }],
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];

    // Should have prowlarr-definitions volume mount
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(
        mounts.iter().any(|m| m.name == "prowlarr-definitions"
            && m.mount_path == "/config/Definitions/Custom"
            && m.read_only == Some(true)),
        "Prowlarr with custom_definitions should have definitions volume mount"
    );

    // Should have prowlarr-definitions volume
    let volumes = pod_spec.volumes.as_ref().unwrap();
    assert!(
        volumes.iter().any(|v| v.name == "prowlarr-definitions"),
        "Should have prowlarr-definitions volume"
    );
}

#[test]
fn test_deployment_custom_resources() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-res".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            resources: Some(ResourceRequirements {
                limits: ResourceList {
                    cpu: "2".into(),
                    memory: "1Gi".into(),
                },
                requests: ResourceList {
                    cpu: "500m".into(),
                    memory: "256Mi".into(),
                },
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    let resources = container.resources.as_ref().unwrap();

    let limits = resources.limits.as_ref().unwrap();
    assert_eq!(limits["cpu"].0, "2");
    assert_eq!(limits["memory"].0, "1Gi");

    let requests = resources.requests.as_ref().unwrap();
    assert_eq!(requests["cpu"].0, "500m");
    assert_eq!(requests["memory"].0, "256Mi");
}

#[test]
fn test_deployment_custom_probes() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("uid-probes".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            probes: Some(ProbeSpec {
                liveness: ProbeConfig {
                    probe_type: ProbeType::Tcp,
                    initial_delay_seconds: 60,
                    period_seconds: 20,
                    timeout_seconds: 5,
                    failure_threshold: 5,
                    ..Default::default()
                },
                readiness: ProbeConfig {
                    probe_type: ProbeType::Tcp,
                    initial_delay_seconds: 15,
                    period_seconds: 5,
                    timeout_seconds: 2,
                    failure_threshold: 3,
                    ..Default::default()
                },
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];

    let liveness = container.liveness_probe.as_ref().unwrap();
    assert!(
        liveness.tcp_socket.is_some(),
        "Liveness should be TCP probe"
    );
    assert_eq!(liveness.initial_delay_seconds, Some(60));
    assert_eq!(liveness.period_seconds, Some(20));

    let readiness = container.readiness_probe.as_ref().unwrap();
    assert!(
        readiness.tcp_socket.is_some(),
        "Readiness should be TCP probe"
    );
    assert_eq!(readiness.initial_delay_seconds, Some(15));
}

#[test]
fn test_deployment_gpu_resources() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("jellyfin".into()),
            namespace: Some("media".into()),
            uid: Some("uid-gpu".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Jellyfin,
            gpu: Some(GpuSpec {
                nvidia: Some(1),
                intel: Some(1),
                amd: None,
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    let resources = container.resources.as_ref().unwrap();

    let limits = resources.limits.as_ref().unwrap();
    assert_eq!(limits["nvidia.com/gpu"].0, "1");
    assert_eq!(limits["gpu.intel.com/i915"].0, "1");
    assert!(
        !limits.contains_key("amd.com/gpu"),
        "AMD GPU should not be present when None"
    );

    let requests = resources.requests.as_ref().unwrap();
    assert_eq!(requests["nvidia.com/gpu"].0, "1");
    assert_eq!(requests["gpu.intel.com/i915"].0, "1");
}

// ============================================================
// NetworkPolicy coverage tests
// ============================================================

#[test]
fn test_networkpolicy_gateway_namespace_ingress() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sonarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-np-gw".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                route_type: RouteType::Http,
                parent_refs: vec![GatewayParentRef {
                    name: "my-gateway".into(),
                    namespace: "gateway-ns".into(),
                    section_name: String::new(),
                }],
                hosts: vec!["sonarr.example.com".into()],
                tls: None,
            }),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let ingress = spec.ingress.unwrap();

    // Should have a rule allowing from gateway namespace
    let gw_rule = ingress.iter().find(|r| {
        r.from.as_ref().is_some_and(|peers| {
            peers.iter().any(|p| {
                p.namespace_selector.as_ref().is_some_and(|ns| {
                    ns.match_labels.as_ref().is_some_and(|labels| {
                        labels.get("kubernetes.io/metadata.name") == Some(&"gateway-ns".to_string())
                    })
                })
            })
        })
    });
    assert!(
        gw_rule.is_some(),
        "Should have ingress rule for gateway namespace"
    );
}

#[test]
fn test_networkpolicy_ssh_bastion_ingress() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-np-ssh".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let ingress = spec.ingress.unwrap();

    // Should have a rule allowing from 0.0.0.0/0
    let ssh_rule = ingress.iter().find(|r| {
        r.from.as_ref().is_some_and(|peers| {
            peers
                .iter()
                .any(|p| p.ip_block.as_ref().is_some_and(|ip| ip.cidr == "0.0.0.0/0"))
        })
    });
    assert!(
        ssh_rule.is_some(),
        "SSH bastion should allow ingress from 0.0.0.0/0"
    );
}

#[test]
fn test_networkpolicy_transmission_peer_port() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("transmission".into()),
            namespace: Some("media".into()),
            uid: Some("uid-np-peer".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Transmission,
            app_config: Some(AppConfig::Transmission(TransmissionConfig {
                peer_port: Some(PeerPortConfig {
                    port: 51413,
                    host_port: false,
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let ingress = spec.ingress.unwrap();

    // Should have a peer port rule with both TCP and UDP
    let peer_rule = ingress.iter().find(|r| {
        r.ports.as_ref().is_some_and(|ports| {
            ports.len() == 2
                && ports.iter().any(|p| p.protocol.as_deref() == Some("TCP"))
                && ports.iter().any(|p| p.protocol.as_deref() == Some("UDP"))
        })
    });
    assert!(
        peer_rule.is_some(),
        "Transmission with peer_port should have TCP+UDP ingress rule"
    );
}

#[test]
fn test_networkpolicy_internet_egress_default_denied_cidrs() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sonarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-np-egress".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            network_policy_config: Some(NetworkPolicyConfig {
                allow_same_namespace: true,
                allow_dns: true,
                allow_internet_egress: true,
                denied_cidr_blocks: vec![],
                custom_egress_rules: vec![],
            }),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let egress = spec.egress.unwrap();

    // Should have internet egress rule with default denied CIDRs
    let internet_rule = egress.iter().find(|r| {
        r.to.as_ref().is_some_and(|peers| {
            peers.iter().any(|p| {
                p.ip_block.as_ref().is_some_and(|ip| {
                    ip.cidr == "0.0.0.0/0"
                        && ip.except.as_ref().is_some_and(|e| {
                            e.contains(&"10.0.0.0/8".to_string())
                                && e.contains(&"172.16.0.0/12".to_string())
                                && e.contains(&"192.168.0.0/16".to_string())
                                && e.contains(&"169.254.0.0/16".to_string())
                        })
                })
            })
        })
    });
    assert!(
        internet_rule.is_some(),
        "Internet egress should use default denied CIDRs including link-local"
    );
}

#[test]
fn test_networkpolicy_internet_egress_custom_denied_cidrs() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sonarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-np-custom-cidr".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            network_policy_config: Some(NetworkPolicyConfig {
                allow_same_namespace: true,
                allow_dns: true,
                allow_internet_egress: true,
                denied_cidr_blocks: vec!["10.0.0.0/8".into(), "192.168.0.0/16".into()],
                custom_egress_rules: vec![],
            }),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let egress = spec.egress.unwrap();

    let internet_rule = egress.iter().find(|r| {
        r.to.as_ref().is_some_and(|peers| {
            peers.iter().any(|p| {
                p.ip_block.as_ref().is_some_and(|ip| {
                    ip.cidr == "0.0.0.0/0"
                        && ip.except.as_ref().is_some_and(|e| {
                            e.len() == 2
                                && e.contains(&"10.0.0.0/8".to_string())
                                && e.contains(&"192.168.0.0/16".to_string())
                        })
                })
            })
        })
    });
    assert!(
        internet_rule.is_some(),
        "Internet egress should use custom denied CIDRs"
    );
}

#[test]
fn test_networkpolicy_custom_egress_rules() {
    let custom_rule = serde_json::json!({
        "to": [{
            "ipBlock": {
                "cidr": "10.0.0.0/8"
            }
        }],
        "ports": [{
            "protocol": "TCP",
            "port": 443
        }]
    });

    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sonarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-np-custom-egress".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            network_policy_config: Some(NetworkPolicyConfig {
                allow_same_namespace: true,
                allow_dns: true,
                allow_internet_egress: false,
                denied_cidr_blocks: vec![],
                custom_egress_rules: vec![custom_rule],
            }),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let egress = spec.egress.unwrap();

    // Should have the custom egress rule (same-ns + DNS + custom = 3 rules)
    assert_eq!(
        egress.len(),
        3,
        "Should have same-ns, DNS, and custom egress rules"
    );

    let custom = &egress[2];
    let to = custom.to.as_ref().unwrap();
    assert!(
        to[0]
            .ip_block
            .as_ref()
            .is_some_and(|ip| ip.cidr == "10.0.0.0/8")
    );
}

#[test]
fn test_networkpolicy_ssh_bastion_nfs_egress() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-np-nfs".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![],
                ..Default::default()
            })),
            persistence: Some(PersistenceSpec {
                volumes: vec![PvcVolume {
                    name: "host-keys".into(),
                    mount_path: "/etc/ssh/keys".into(),
                    access_mode: "ReadWriteOnce".into(),
                    size: "10Mi".into(),
                    storage_class: String::new(),
                }],
                nfs_mounts: vec![NfsMount {
                    name: "media".into(),
                    server: "192.168.1.100".into(),
                    path: "/exports/media".into(),
                    mount_path: "/media".into(),
                    read_only: true,
                }],
            }),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let egress = spec.egress.unwrap();

    // Should have an NFS egress rule (port 2049 TCP to private CIDRs)
    let nfs_rule = egress.iter().find(|r| {
        r.ports.as_ref().is_some_and(|ports| {
            ports.iter().any(|p| {
                p.port == Some(k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(2049))
            })
        })
    });
    assert!(
        nfs_rule.is_some(),
        "SSH bastion with NFS mounts should have NFS egress rule"
    );
}

#[test]
fn test_networkpolicy_allow_dns_false() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sonarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-np-nodns".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            network_policy_config: Some(NetworkPolicyConfig {
                allow_same_namespace: true,
                allow_dns: false,
                allow_internet_egress: false,
                denied_cidr_blocks: vec![],
                custom_egress_rules: vec![],
            }),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let egress = spec.egress.unwrap();

    // Only same-namespace egress, no DNS rule
    assert_eq!(
        egress.len(),
        1,
        "With allow_dns=false, should only have same-ns egress"
    );

    // Verify no DNS port 53 rule
    let has_dns = egress.iter().any(|r| {
        r.ports.as_ref().is_some_and(|ports| {
            ports.iter().any(|p| {
                p.port == Some(k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(53))
            })
        })
    });
    assert!(
        !has_dns,
        "Should not have DNS egress rule when allow_dns=false"
    );
}

// ============================================================
// common.rs coverage tests
// ============================================================

#[test]
fn test_common_app_name_returns_metadata_name() {
    let app = make_app(AppType::Sonarr);
    let name = servarr_resources::common::app_name(&app);
    assert_eq!(name, "test-app");
}

#[test]
fn test_common_app_name_returns_unknown_when_no_name() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: None,
            namespace: Some("media".into()),
            uid: Some("uid-noname".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            ..Default::default()
        },
        status: None,
    };
    let name = servarr_resources::common::app_name(&app);
    assert_eq!(name, "unknown");
}

#[test]
fn test_common_app_namespace_returns_metadata_namespace() {
    let app = make_app(AppType::Sonarr);
    let ns = servarr_resources::common::app_namespace(&app);
    assert_eq!(ns, "media");
}

#[test]
fn test_common_app_namespace_returns_default_when_no_ns() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: None,
            uid: Some("uid-nons".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            ..Default::default()
        },
        status: None,
    };
    let ns = servarr_resources::common::app_namespace(&app);
    assert_eq!(ns, "default");
}

#[test]
fn test_common_labels_basic() {
    let app = make_app(AppType::Radarr);
    let labels = servarr_resources::common::labels(&app);

    assert_eq!(labels["app.kubernetes.io/name"], "radarr");
    assert_eq!(labels["app.kubernetes.io/instance"], "test-app");
    assert_eq!(labels["app.kubernetes.io/managed-by"], "servarr-operator");
    assert_eq!(labels["servarr.dev/app"], "radarr");
    // No instance label when spec.instance is None
    assert!(!labels.contains_key("servarr.dev/instance"));
}

#[test]
fn test_common_labels_with_instance() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sonarr-4k".into()),
            namespace: Some("media".into()),
            uid: Some("uid-inst".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            instance: Some("4k".into()),
            ..Default::default()
        },
        status: None,
    };
    let labels = servarr_resources::common::labels(&app);
    assert_eq!(labels["servarr.dev/instance"], "4k");
}

#[test]
fn test_common_selector_labels() {
    let app = make_app(AppType::Lidarr);
    let sel = servarr_resources::common::selector_labels(&app);

    assert_eq!(sel.len(), 2);
    assert_eq!(sel["app.kubernetes.io/name"], "lidarr");
    assert_eq!(sel["app.kubernetes.io/instance"], "test-app");
}

#[test]
fn test_common_child_name_empty_suffix() {
    let app = make_app(AppType::Sonarr);
    let name = servarr_resources::common::child_name(&app, "");
    assert_eq!(name, "test-app");
}

#[test]
fn test_common_child_name_with_suffix() {
    let app = make_app(AppType::Sonarr);
    let name = servarr_resources::common::child_name(&app, "config");
    assert_eq!(name, "test-app-config");
}

#[test]
fn test_common_name_for_alias() {
    let app = make_app(AppType::Sonarr);
    let name = servarr_resources::common::name_for(&app, "downloads");
    assert_eq!(name, "test-app-downloads");
}

#[test]
fn test_common_namespace_alias() {
    let app = make_app(AppType::Sonarr);
    let ns = servarr_resources::common::namespace(&app);
    assert_eq!(ns, "media");
}

#[test]
fn test_common_metadata_sets_all_fields() {
    let app = make_app(AppType::Sonarr);
    let meta = servarr_resources::common::metadata(&app, "config");

    assert_eq!(meta.name.as_deref(), Some("test-app-config"));
    assert_eq!(meta.namespace.as_deref(), Some("media"));
    assert!(meta.labels.is_some());
    assert!(meta.owner_references.is_some());

    let owner_refs = meta.owner_references.unwrap();
    assert_eq!(owner_refs.len(), 1);
    assert_eq!(owner_refs[0].uid, "test-uid-123");
}

#[test]
fn test_common_metadata_no_suffix() {
    let app = make_app(AppType::Sonarr);
    let meta = servarr_resources::common::metadata(&app, "");
    assert_eq!(meta.name.as_deref(), Some("test-app"));
}

#[test]
fn test_common_owner_reference() {
    let app = make_app(AppType::Sonarr);
    let owner_ref = servarr_resources::common::owner_reference(&app);
    assert_eq!(owner_ref.uid, "test-uid-123");
    assert!(owner_ref.controller.unwrap_or(false));
}

#[test]
fn test_common_owner_ref_alias() {
    let app = make_app(AppType::Sonarr);
    let owner_ref = servarr_resources::common::owner_ref(&app);
    assert_eq!(owner_ref.uid, "test-uid-123");
}

// ============================================================
// service.rs coverage tests
// ============================================================

#[test]
fn test_service_builder_radarr_default_port() {
    let app = make_app(AppType::Radarr);
    let svc = servarr_resources::service::build(&app);

    let spec = svc.spec.unwrap();
    let ports = spec.ports.unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].port, 7878);
    assert_eq!(ports[0].name.as_deref(), Some("http"));
    assert_eq!(ports[0].protocol.as_deref(), Some("TCP"));
}

#[test]
fn test_service_builder_sonarr_default_port() {
    let app = make_app(AppType::Sonarr);
    let svc = servarr_resources::service::build(&app);

    let spec = svc.spec.unwrap();
    let ports = spec.ports.unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].port, 8989);
}

#[test]
fn test_service_builder_prowlarr_default_port() {
    let app = make_app(AppType::Prowlarr);
    let svc = servarr_resources::service::build(&app);

    let spec = svc.spec.unwrap();
    let ports = spec.ports.unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].port, 9696);
}

#[test]
fn test_service_builder_transmission_with_peer_port() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("transmission".into()),
            namespace: Some("media".into()),
            uid: Some("uid-svc-tx".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Transmission,
            app_config: Some(AppConfig::Transmission(TransmissionConfig {
                peer_port: Some(PeerPortConfig {
                    port: 51413,
                    host_port: false,
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let svc = servarr_resources::service::build(&app);
    let spec = svc.spec.unwrap();
    let ports = spec.ports.unwrap();

    // Should have the main port + peer-tcp + peer-udp = 3 ports
    assert_eq!(ports.len(), 3);
    assert!(
        ports
            .iter()
            .any(|p| p.name.as_deref() == Some("peer-tcp") && p.port == 51413)
    );
    assert!(
        ports
            .iter()
            .any(|p| p.name.as_deref() == Some("peer-udp") && p.port == 51413)
    );
}

#[test]
fn test_service_builder_selector_labels() {
    let app = make_app(AppType::Sonarr);
    let svc = servarr_resources::service::build(&app);

    let spec = svc.spec.unwrap();
    let selector = spec.selector.unwrap();
    assert_eq!(selector["app.kubernetes.io/name"], "sonarr");
    assert_eq!(selector["app.kubernetes.io/instance"], "test-app");
}

#[test]
fn test_service_builder_clusterip_type() {
    let app = make_app(AppType::Sonarr);
    let svc = servarr_resources::service::build(&app);

    let spec = svc.spec.unwrap();
    assert_eq!(spec.type_.as_deref(), Some("ClusterIP"));
}

#[test]
fn test_service_builder_owner_references() {
    let app = make_app(AppType::Sonarr);
    let svc = servarr_resources::service::build(&app);

    let owner_refs = svc.metadata.owner_references.unwrap();
    assert_eq!(owner_refs.len(), 1);
    assert_eq!(owner_refs[0].uid, "test-uid-123");
}

#[test]
fn test_networkpolicy_allow_same_namespace_false() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("sonarr".into()),
            namespace: Some("media".into()),
            uid: Some("uid-np-nons".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            network_policy_config: Some(NetworkPolicyConfig {
                allow_same_namespace: false,
                allow_dns: true,
                allow_internet_egress: false,
                denied_cidr_blocks: vec![],
                custom_egress_rules: vec![],
            }),
            ..Default::default()
        },
        status: None,
    };

    let np = servarr_resources::networkpolicy::build(&app);
    let spec = np.spec.unwrap();
    let ingress = spec.ingress.unwrap();

    // With allow_same_namespace=false, no gateway, no SSH, no peer port:
    // ingress should be empty
    assert!(
        ingress.is_empty(),
        "With allow_same_namespace=false and no other ingress sources, ingress rules should be empty"
    );
}

// ---------------------------------------------------------------------------
// SSH Bastion advanced env vars (tcp_forwarding, gateway_ports, disable_sftp, motd)
// ---------------------------------------------------------------------------

#[test]
fn test_deployment_ssh_bastion_advanced_env_vars() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion-advanced".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-adv".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "admin".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::Shell,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                tcp_forwarding: true,
                gateway_ports: true,
                disable_sftp: true,
                motd: "Welcome to bastion".into(),
                sftp_chroot: "/chroot".into(),
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];
    let env = container.env.as_ref().unwrap();

    let find_env = |name: &str| -> Option<String> {
        env.iter()
            .find(|e| e.name == name)
            .and_then(|e| e.value.clone())
    };

    assert_eq!(find_env("TCP_FORWARDING"), Some("true".into()));
    assert_eq!(find_env("GATEWAY_PORTS"), Some("true".into()));
    assert_eq!(find_env("SFTP_MODE"), Some("false".into()));
    assert_eq!(find_env("MOTD"), Some("Welcome to bastion".into()));
    assert_eq!(find_env("SFTP_CHROOT"), Some("/chroot".into()));
}

#[test]
fn test_deployment_ssh_bastion_managed_env_ignored() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion-env".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-env".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "user1".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::Shell,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            env: vec![
                servarr_crds::EnvVar {
                    name: "SSH_USERS".into(),
                    value: "SHOULD_BE_IGNORED".into(),
                },
                servarr_crds::EnvVar {
                    name: "CUSTOM_VAR".into(),
                    value: "allowed".into(),
                },
            ],
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];
    let env = container.env.as_ref().unwrap();

    // SSH_USERS should be set by the operator, not the user override
    let ssh_users = env.iter().find(|e| e.name == "SSH_USERS").unwrap();
    assert_ne!(
        ssh_users.value.as_deref(),
        Some("SHOULD_BE_IGNORED"),
        "SSH_USERS should not accept user override"
    );

    // CUSTOM_VAR should be allowed
    let custom = env.iter().find(|e| e.name == "CUSTOM_VAR").unwrap();
    assert_eq!(custom.value.as_deref(), Some("allowed"));
}

#[test]
fn test_deployment_ssh_bastion_host_keys_preserved_with_nfs_mounts() {
    // Regression test: when a ServarrApp for SshBastion has persistence set
    // (e.g. injected by a MediaStack with NFS mounts in stack defaults), the
    // app-type-default host-keys PVC must still appear in the deployment.
    // Previously, the host-keys volume was dropped because deployment.rs used
    // unwrap_or — taking the spec persistence wholesale and discarding the
    // app-type defaults entirely.
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("media-ssh-bastion".into()),
            namespace: Some("media".into()),
            uid: Some("uid-ssh-nfs".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            // Persistence set by MediaStack stack defaults: NFS mounts only,
            // no explicit PVC volumes.
            persistence: Some(PersistenceSpec {
                volumes: vec![],
                nfs_mounts: vec![
                    NfsMount {
                        name: "media".into(),
                        server: "nas.local".into(),
                        path: "/volume1/media".into(),
                        mount_path: "/media".into(),
                        read_only: false,
                    },
                    NfsMount {
                        name: "downloads".into(),
                        server: "nas.local".into(),
                        path: "/volume1/downloads".into(),
                        mount_path: "/downloads".into(),
                        read_only: false,
                    },
                ],
            }),
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "admin".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::Sftp,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    let volumes = pod_spec.volumes.as_ref().expect("pod should have volumes");

    // host-keys PVC must be present despite NFS mounts being set in the spec.
    assert!(
        volumes.iter().any(|v| v.name == "host-keys"),
        "host-keys PVC volume must be present even when spec.persistence contains NFS mounts; \
         got volumes: {:?}",
        volumes.iter().map(|v| &v.name).collect::<Vec<_>>()
    );

    // NFS mounts from the spec must also be present.
    assert!(
        volumes.iter().any(|v| v.name == "nfs-media"),
        "nfs-media volume must be present"
    );
    assert!(
        volumes.iter().any(|v| v.name == "nfs-downloads"),
        "nfs-downloads volume must be present"
    );

    // generate-host-keys init container must be present and able to mount host-keys.
    let init = pod_spec
        .init_containers
        .as_ref()
        .expect("should have init containers");
    assert!(
        init.iter().any(|c| c.name == "generate-host-keys"),
        "generate-host-keys init container must be present"
    );
}

// ---------------------------------------------------------------------------
// NFS server resource builders
// ---------------------------------------------------------------------------

fn make_owner_ref() -> k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
    k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
        api_version: "servarr.dev/v1alpha1".to_string(),
        kind: "MediaStack".to_string(),
        name: "mystack".to_string(),
        uid: "stack-uid-1".to_string(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

#[test]
fn test_nfs_server_statefulset_name_and_namespace() {
    let nfs = NfsServerSpec::default();
    let ss = servarr_resources::nfs_server::build_statefulset(
        "mystack",
        "media",
        &nfs,
        make_owner_ref(),
    );
    assert_eq!(ss.metadata.name.as_deref(), Some("mystack-nfs-server"));
    assert_eq!(ss.metadata.namespace.as_deref(), Some("media"));
}

#[test]
fn test_nfs_server_statefulset_replicas_and_service_name() {
    let nfs = NfsServerSpec::default();
    let ss = servarr_resources::nfs_server::build_statefulset(
        "mystack",
        "media",
        &nfs,
        make_owner_ref(),
    );
    let spec = ss.spec.unwrap();
    assert_eq!(spec.replicas, Some(1));
    assert_eq!(spec.service_name.as_deref(), Some("mystack-nfs-server"));
}

#[test]
fn test_nfs_server_statefulset_port_2049() {
    let nfs = NfsServerSpec::default();
    let ss = servarr_resources::nfs_server::build_statefulset(
        "mystack",
        "media",
        &nfs,
        make_owner_ref(),
    );
    let spec = ss.spec.unwrap();
    let pod_spec = spec.template.spec.unwrap();
    let container = &pod_spec.containers[0];
    let ports = container.ports.as_ref().unwrap();
    assert!(
        ports.iter().any(|p| p.container_port == 2049),
        "NFS server must expose port 2049"
    );
}

#[test]
fn test_nfs_server_statefulset_export_dir_mount() {
    let nfs = NfsServerSpec::default();
    let ss = servarr_resources::nfs_server::build_statefulset(
        "mystack",
        "media",
        &nfs,
        make_owner_ref(),
    );
    let spec = ss.spec.unwrap();
    let pod_spec = spec.template.spec.unwrap();
    let container = &pod_spec.containers[0];
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(
        mounts.iter().any(|m| m.mount_path == "/nfsshare"),
        "data volume must be mounted at /nfsshare"
    );
}

#[test]
fn test_nfs_server_statefulset_volume_claim_template_storage_size() {
    let nfs = NfsServerSpec {
        storage_size: "500Gi".to_string(),
        storage_class: Some("fast-ssd".to_string()),
        ..Default::default()
    };
    let ss = servarr_resources::nfs_server::build_statefulset(
        "mystack",
        "media",
        &nfs,
        make_owner_ref(),
    );
    let spec = ss.spec.unwrap();
    let vclaim = &spec.volume_claim_templates.unwrap()[0];
    let pvc_spec = vclaim.spec.as_ref().unwrap();
    assert_eq!(pvc_spec.storage_class_name.as_deref(), Some("fast-ssd"));
    let storage = pvc_spec
        .resources
        .as_ref()
        .unwrap()
        .requests
        .as_ref()
        .unwrap()
        .get("storage")
        .unwrap();
    assert_eq!(storage.0, "500Gi");
}

#[test]
fn test_nfs_server_statefulset_custom_image() {
    let nfs = NfsServerSpec {
        image: Some(ImageSpec {
            repository: "my-registry/nfs-server".to_string(),
            tag: "v2".to_string(),
            digest: String::new(),
            pull_policy: "IfNotPresent".to_string(),
        }),
        ..Default::default()
    };
    let ss = servarr_resources::nfs_server::build_statefulset(
        "mystack",
        "media",
        &nfs,
        make_owner_ref(),
    );
    let spec = ss.spec.unwrap();
    let container = &spec.template.spec.unwrap().containers[0];
    assert_eq!(
        container.image.as_deref(),
        Some("my-registry/nfs-server:v2")
    );
}

#[test]
fn test_nfs_server_service_name_and_namespace() {
    let svc = servarr_resources::nfs_server::build_service("mystack", "media", make_owner_ref());
    assert_eq!(svc.metadata.name.as_deref(), Some("mystack-nfs-server"));
    assert_eq!(svc.metadata.namespace.as_deref(), Some("media"));
}

#[test]
fn test_nfs_server_service_port_2049() {
    let svc = servarr_resources::nfs_server::build_service("mystack", "media", make_owner_ref());
    let spec = svc.spec.unwrap();
    // Headless service (clusterIP: None) so DNS returns the pod IP directly,
    // allowing the kubelet to reach the NFS server without cluster DNS.
    assert_eq!(spec.cluster_ip.as_deref(), Some("None"));
    let ports = spec.ports.unwrap();
    assert!(
        ports.iter().any(|p| p.port == 2049),
        "NFS service must expose port 2049"
    );
}

#[test]
fn test_nfs_server_service_selector_labels() {
    let svc = servarr_resources::nfs_server::build_service("mystack", "media", make_owner_ref());
    let selector = svc.spec.unwrap().selector.unwrap();
    assert_eq!(
        selector.get("servarr.dev/stack").map(|s| s.as_str()),
        Some("mystack")
    );
    assert_eq!(
        selector.get("servarr.dev/component").map(|s| s.as_str()),
        Some("nfs-server")
    );
}

#[test]
fn test_deployment_no_gpu_no_node_selector() {
    let app = make_app(AppType::Sonarr);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    assert!(
        pod_spec.node_selector.is_none(),
        "no GPU spec should produce no nodeSelector"
    );
}

#[test]
fn test_deployment_intel_gpu_adds_nfd_node_selector() {
    let mut app = make_app(AppType::Jellyfin);
    app.spec.gpu = Some(servarr_crds::GpuSpec {
        intel: Some(1),
        ..Default::default()
    });
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let sel = pod_spec
        .node_selector
        .expect("nodeSelector must be set for Intel GPU");
    assert_eq!(
        sel.get("gpu.intel.com/i915").map(|s| s.as_str()),
        Some("true")
    );
    assert!(!sel.contains_key("gpu.nvidia.com/present"));
    assert!(!sel.contains_key("gpu.amd.com/present"));
}

#[test]
fn test_deployment_nvidia_gpu_adds_nfd_node_selector() {
    let mut app = make_app(AppType::Jellyfin);
    app.spec.gpu = Some(servarr_crds::GpuSpec {
        nvidia: Some(1),
        ..Default::default()
    });
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let sel = pod_spec
        .node_selector
        .expect("nodeSelector must be set for NVIDIA GPU");
    assert_eq!(
        sel.get("gpu.nvidia.com/present").map(|s| s.as_str()),
        Some("true")
    );
    assert!(!sel.contains_key("gpu.intel.com/i915"));
    assert!(!sel.contains_key("gpu.amd.com/present"));
}

#[test]
fn test_deployment_amd_gpu_adds_nfd_node_selector() {
    let mut app = make_app(AppType::Jellyfin);
    app.spec.gpu = Some(servarr_crds::GpuSpec {
        amd: Some(1),
        ..Default::default()
    });
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let sel = pod_spec
        .node_selector
        .expect("nodeSelector must be set for AMD GPU");
    assert_eq!(
        sel.get("gpu.amd.com/present").map(|s| s.as_str()),
        Some("true")
    );
    assert!(!sel.contains_key("gpu.intel.com/i915"));
    assert!(!sel.contains_key("gpu.nvidia.com/present"));
}

#[test]
fn test_deployment_user_node_selector_preserved_with_gpu() {
    let mut app = make_app(AppType::Jellyfin);
    app.spec.gpu = Some(servarr_crds::GpuSpec {
        intel: Some(1),
        ..Default::default()
    });
    app.spec.scheduling = Some(servarr_crds::NodeScheduling {
        node_selector: std::collections::BTreeMap::from([(
            "kubernetes.io/hostname".into(),
            "my-node".into(),
        )]),
        ..Default::default()
    });
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let sel = pod_spec.node_selector.expect("nodeSelector must be set");
    assert_eq!(
        sel.get("gpu.intel.com/i915").map(|s| s.as_str()),
        Some("true")
    );
    assert_eq!(
        sel.get("kubernetes.io/hostname").map(|s| s.as_str()),
        Some("my-node")
    );
}

// ---------------------------------------------------------------------------
// adminCredentials env var injection
// ---------------------------------------------------------------------------

fn find_env<'a>(
    env: &'a [k8s_openapi::api::core::v1::EnvVar],
    name: &str,
) -> Option<&'a k8s_openapi::api::core::v1::EnvVar> {
    env.iter().find(|e| e.name == name)
}

fn get_env(app: &ServarrApp) -> Vec<k8s_openapi::api::core::v1::EnvVar> {
    let deploy = servarr_resources::deployment::build(app, &std::collections::HashMap::new());
    deploy
        .spec
        .unwrap()
        .template
        .spec
        .unwrap()
        .containers
        .into_iter()
        .next()
        .unwrap()
        .env
        .unwrap_or_default()
}

#[test]
fn test_admin_credentials_sonarr_no_auth_env_vars() {
    // Auth is configured via PUT /api/v3/config/host, not env vars.
    // Env var injection caused Sonarr to store plaintext passwords (not BCrypt-hashed),
    // making login always fail.
    let mut app = make_app(AppType::Sonarr);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "my-admin".into(),
    });
    let env = get_env(&app);

    assert!(find_env(&env, "SONARR__AUTH__USERNAME").is_none());
    assert!(find_env(&env, "SONARR__AUTH__PASSWORD").is_none());
    assert!(find_env(&env, "SONARR__AUTH__METHOD").is_none());
}

#[test]
fn test_admin_credentials_radarr_no_auth_env_vars() {
    let mut app = make_app(AppType::Radarr);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "radarr-creds".into(),
    });
    let env = get_env(&app);

    assert!(find_env(&env, "RADARR__AUTH__USERNAME").is_none());
    assert!(find_env(&env, "RADARR__AUTH__PASSWORD").is_none());
    assert!(find_env(&env, "RADARR__AUTH__METHOD").is_none());
}

#[test]
fn test_admin_credentials_lidarr_no_auth_env_vars() {
    let mut app = make_app(AppType::Lidarr);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "lidarr-creds".into(),
    });
    let env = get_env(&app);

    assert!(find_env(&env, "LIDARR__AUTH__USERNAME").is_none());
    assert!(find_env(&env, "LIDARR__AUTH__PASSWORD").is_none());
    assert!(find_env(&env, "LIDARR__AUTH__METHOD").is_none());
}

#[test]
fn test_admin_credentials_prowlarr_no_auth_env_vars() {
    let mut app = make_app(AppType::Prowlarr);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "prowlarr-creds".into(),
    });
    let env = get_env(&app);

    assert!(find_env(&env, "PROWLARR__AUTH__USERNAME").is_none());
    assert!(find_env(&env, "PROWLARR__AUTH__PASSWORD").is_none());
    assert!(find_env(&env, "PROWLARR__AUTH__METHOD").is_none());
}

#[test]
fn test_admin_credentials_transmission_env_vars() {
    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "creds".into(),
    });
    let env = get_env(&app);

    // Sonarr-style env vars must NOT be set for Transmission
    assert!(find_env(&env, "TRANSMISSION__AUTH__USERNAME").is_none());
    assert!(find_env(&env, "TRANSMISSION__AUTH__PASSWORD").is_none());
    assert!(find_env(&env, "TRANSMISSION__AUTH__METHOD").is_none());

    // FILE__USER / FILE__PASS: LSIO FILE__ mechanism for s6 container env
    let file_user = find_env(&env, "FILE__USER").expect("FILE__USER must be set for Transmission");
    let file_pass = find_env(&env, "FILE__PASS").expect("FILE__PASS must be set for Transmission");
    assert_eq!(
        file_user.value.as_deref(),
        Some("/run/secrets/admin/username"),
        "FILE__USER must point to the mounted secret file"
    );
    assert_eq!(
        file_pass.value.as_deref(),
        Some("/run/secrets/admin/password"),
        "FILE__PASS must point to the mounted secret file"
    );

    // USER/PASS secretKeyRef kept for exec probe (curl -u "$USER:$PASS")
    let user = find_env(&env, "USER").expect("USER env var should be set for Transmission");
    let pass = find_env(&env, "PASS").expect("PASS env var should be set for Transmission");
    assert_eq!(
        user.value_from
            .as_ref()
            .and_then(|v| v.secret_key_ref.as_ref())
            .map(|s| s.key.as_str()),
        Some("username"),
        "USER should be sourced from adminCredentials secret key 'username'"
    );
    assert_eq!(
        pass.value_from
            .as_ref()
            .and_then(|v| v.secret_key_ref.as_ref())
            .map(|s| s.key.as_str()),
        Some("password"),
        "PASS should be sourced from adminCredentials secret key 'password'"
    );
}

#[test]
fn test_admin_credentials_transmission_mounts_secret_volume() {
    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "creds".into(),
    });
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    // Volume must exist
    let vol = pod_spec
        .volumes
        .as_ref()
        .and_then(|vs| vs.iter().find(|v| v.name == "admin-credentials"))
        .expect("admin-credentials volume must be present");
    assert_eq!(
        vol.secret.as_ref().and_then(|s| s.secret_name.as_deref()),
        Some("creds"),
        "admin-credentials volume must reference the adminCredentials secret"
    );

    // Volume mount must exist in the container
    let mounts = pod_spec.containers[0]
        .volume_mounts
        .as_ref()
        .expect("container must have volume mounts");
    let mount = mounts
        .iter()
        .find(|m| m.name == "admin-credentials")
        .expect("admin-credentials mount must be present");
    assert_eq!(mount.mount_path, "/run/secrets/admin");
    assert_eq!(mount.read_only, Some(true));
}

#[test]
fn test_admin_credentials_transmission_mounts_auth_script_to_custom_cont_init() {
    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "creds".into(),
    });
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    // The custom-cont-init.d script must be mounted in the main container via subPath.
    // This guarantees the auth script runs AFTER init-transmission-config
    // (confirmed by s6-rc dependency chain: init-transmission-config →
    // init-config-end → init-mods → init-mods-package-install →
    // init-mods-end → init-custom-files).
    let mounts = pod_spec.containers[0]
        .volume_mounts
        .as_ref()
        .expect("container must have volume mounts");
    let auth_mount = mounts
        .iter()
        .find(|m| m.mount_path == "/custom-cont-init.d/99-transmission-auth.sh")
        .expect("99-transmission-auth.sh must be mounted at /custom-cont-init.d/");
    assert_eq!(auth_mount.name, "scripts");
    assert_eq!(
        auth_mount.sub_path.as_deref(),
        Some("99-transmission-auth.sh")
    );
    assert_eq!(auth_mount.read_only, Some(true));

    // The script key must also be present in the scripts volume items
    let scripts_vol = pod_spec
        .volumes
        .as_ref()
        .and_then(|vs| vs.iter().find(|v| v.name == "scripts"))
        .expect("scripts volume must be present");
    let items = scripts_vol
        .config_map
        .as_ref()
        .and_then(|cm| cm.items.as_ref())
        .expect("scripts volume must have items");
    assert!(
        items.iter().any(|i| i.key == "99-transmission-auth.sh"),
        "scripts volume items must include 99-transmission-auth.sh"
    );
}

#[test]
fn test_admin_credentials_transmission_uses_exec_probe() {
    use k8s_openapi::api::core::v1::ExecAction;

    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "creds".into(),
    });
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];

    let liveness = container
        .liveness_probe
        .as_ref()
        .expect("liveness probe must be set");
    let readiness = container
        .readiness_probe
        .as_ref()
        .expect("readiness probe must be set");

    // Both probes must be exec (not httpGet) so they send credentials after auth is enabled
    assert!(
        liveness.http_get.is_none(),
        "liveness probe must not be httpGet when adminCredentials is set"
    );
    assert!(
        liveness.exec.is_some(),
        "liveness probe must be exec when adminCredentials is set"
    );
    let exec_cmd = liveness.exec.as_ref().and_then(|e| e.command.as_ref());
    assert!(
        exec_cmd.is_some_and(|c| c.iter().any(|s| s.contains("USER") && s.contains("PASS"))),
        "exec probe command must include $USER and $PASS"
    );
    assert!(
        readiness.exec.is_some(),
        "readiness probe must be exec when adminCredentials is set"
    );
    let _ = ExecAction::default(); // suppress unused import warning
}

#[test]
fn test_admin_credentials_not_set_no_auth_env_vars() {
    let app = make_app(AppType::Sonarr);
    // admin_credentials defaults to None
    let env = get_env(&app);

    assert!(find_env(&env, "SONARR__AUTH__USERNAME").is_none());
    assert!(find_env(&env, "SONARR__AUTH__PASSWORD").is_none());
    assert!(find_env(&env, "SONARR__AUTH__METHOD").is_none());
}

/// Verify that DynamicObject serializes apiVersion and kind correctly after the
/// from_value() round-trip used in httproute::build and certificate::build.
/// If apiVersion/kind are missing, SSA patches fail with "invalid object type: /, Kind=".
#[test]
fn test_dynamic_object_serialization_preserves_type_meta() {
    let route_json = serde_json::json!({
        "apiVersion": "gateway.networking.k8s.io/v1",
        "kind": "HTTPRoute",
        "metadata": {"name": "test", "namespace": "test-ns"},
        "spec": {"rules": []}
    });
    let route: kube::api::DynamicObject =
        serde_json::from_value(route_json).expect("from_value should succeed");
    let serialized = serde_json::to_value(&route).expect("to_value should succeed");

    assert_eq!(
        serialized["apiVersion"].as_str(),
        Some("gateway.networking.k8s.io/v1"),
        "apiVersion must survive DynamicObject round-trip"
    );
    assert_eq!(
        serialized["kind"].as_str(),
        Some("HTTPRoute"),
        "kind must survive DynamicObject round-trip"
    );
}

/// Same check for httproute::build output.
#[test]
fn test_httproute_ssa_body_has_type_meta() {
    use servarr_crds::{GatewayParentRef, GatewaySpec, RouteType};
    let app = ServarrApp {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                route_type: RouteType::Http,
                parent_refs: vec![GatewayParentRef {
                    name: "test-gw".into(),
                    namespace: String::new(),
                    section_name: String::new(),
                }],
                hosts: vec!["sonarr.example.com".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        status: None,
    };

    let route = servarr_resources::httproute::build(&app).expect("should build HTTPRoute");
    let body = serde_json::to_value(&route).expect("should serialize");

    assert_eq!(
        body["apiVersion"].as_str(),
        Some("gateway.networking.k8s.io/v1"),
        "SSA body must contain apiVersion"
    );
    assert_eq!(
        body["kind"].as_str(),
        Some("HTTPRoute"),
        "SSA body must contain kind"
    );
}
