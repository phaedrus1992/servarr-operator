use servarr_crds::*;

// ---------------------------------------------------------------------------
// Tier assignment
// ---------------------------------------------------------------------------

#[test]
fn test_tier_assignment() {
    assert_eq!(AppType::Plex.tier(), 0);
    assert_eq!(AppType::Jellyfin.tier(), 0);
    assert_eq!(AppType::SshBastion.tier(), 0);
    assert_eq!(AppType::Sabnzbd.tier(), 1);
    assert_eq!(AppType::Transmission.tier(), 1);
    assert_eq!(AppType::Sonarr.tier(), 2);
    assert_eq!(AppType::Radarr.tier(), 2);
    assert_eq!(AppType::Lidarr.tier(), 2);
    assert_eq!(AppType::Tautulli.tier(), 3);
    assert_eq!(AppType::Overseerr.tier(), 3);
    assert_eq!(AppType::Maintainerr.tier(), 3);
    assert_eq!(AppType::Prowlarr.tier(), 3);
    assert_eq!(AppType::Jackett.tier(), 3);
}

#[test]
fn test_tier_names() {
    assert_eq!(AppType::tier_name(0), "MediaServers");
    assert_eq!(AppType::tier_name(1), "DownloadClients");
    assert_eq!(AppType::tier_name(2), "MediaManagers");
    assert_eq!(AppType::tier_name(3), "Ancillary");
    assert_eq!(AppType::tier_name(99), "Unknown");
}

// ---------------------------------------------------------------------------
// Child name generation
// ---------------------------------------------------------------------------

#[test]
fn test_child_name_without_instance() {
    let app = StackApp {
        app: AppType::Sonarr,
        instance: None,
        enabled: true,
        image: None,
        uid: None,
        gid: None,
        security: None,
        service: None,
        gateway: None,
        resources: None,
        persistence: None,
        env: Vec::new(),
        probes: None,
        scheduling: None,
        network_policy: None,
        network_policy_config: None,
        app_config: None,
        api_key_secret: None,
        api_health_check: None,
        backup: None,
        image_pull_secrets: None,
        pod_annotations: None,
        gpu: None,
        prowlarr_sync: None,
        overseerr_sync: None,
        bazarr_sync: None,
        subgen_sync: None,
        admin_credentials: None,
        split4k: None,
        split4k_overrides: None,
    };
    assert_eq!(app.child_name("media"), "media-sonarr");
}

#[test]
fn test_child_name_with_instance() {
    let app = StackApp {
        app: AppType::Sonarr,
        instance: Some("4k".into()),
        enabled: true,
        image: None,
        uid: None,
        gid: None,
        security: None,
        service: None,
        gateway: None,
        resources: None,
        persistence: None,
        env: Vec::new(),
        probes: None,
        scheduling: None,
        network_policy: None,
        network_policy_config: None,
        app_config: None,
        api_key_secret: None,
        api_health_check: None,
        backup: None,
        image_pull_secrets: None,
        pod_annotations: None,
        gpu: None,
        prowlarr_sync: None,
        overseerr_sync: None,
        bazarr_sync: None,
        subgen_sync: None,
        admin_credentials: None,
        split4k: None,
        split4k_overrides: None,
    };
    assert_eq!(app.child_name("stack"), "stack-sonarr-4k");
}

// ---------------------------------------------------------------------------
// Helper to create a minimal StackApp
// ---------------------------------------------------------------------------

fn minimal_stack_app(app: AppType) -> StackApp {
    StackApp {
        app,
        instance: None,
        enabled: true,
        image: None,
        uid: None,
        gid: None,
        security: None,
        service: None,
        gateway: None,
        resources: None,
        persistence: None,
        env: Vec::new(),
        probes: None,
        scheduling: None,
        network_policy: None,
        network_policy_config: None,
        app_config: None,
        api_key_secret: None,
        api_health_check: None,
        backup: None,
        image_pull_secrets: None,
        pod_annotations: None,
        gpu: None,
        prowlarr_sync: None,
        overseerr_sync: None,
        bazarr_sync: None,
        subgen_sync: None,
        admin_credentials: None,
        split4k: None,
        split4k_overrides: None,
    }
}

// ---------------------------------------------------------------------------
// Merge: env
// ---------------------------------------------------------------------------

#[test]
fn test_merge_env_app_overrides_stack() {
    let defaults = StackDefaults {
        env: vec![
            EnvVar {
                name: "TZ".into(),
                value: "UTC".into(),
            },
            EnvVar {
                name: "FOO".into(),
                value: "bar".into(),
            },
        ],
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.env = vec![EnvVar {
        name: "TZ".into(),
        value: "America/New_York".into(),
    }];

    let spec = app.to_servarr_spec(Some(&defaults));
    assert_eq!(spec.env.len(), 2);

    let tz = spec.env.iter().find(|e| e.name == "TZ").unwrap();
    assert_eq!(tz.value, "America/New_York");

    let foo = spec.env.iter().find(|e| e.name == "FOO").unwrap();
    assert_eq!(foo.value, "bar");
}

// ---------------------------------------------------------------------------
// Merge: persistence
// ---------------------------------------------------------------------------

#[test]
fn test_merge_persistence_app_pvc_replaces_stack() {
    let defaults = StackDefaults {
        persistence: Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "config".into(),
                mount_path: "/config".into(),
                size: "1Gi".into(),
                ..Default::default()
            }],
            nfs_mounts: Vec::new(),
        }),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.persistence = Some(PersistenceSpec {
        volumes: vec![PvcVolume {
            name: "data".into(),
            mount_path: "/data".into(),
            size: "10Gi".into(),
            ..Default::default()
        }],
        nfs_mounts: Vec::new(),
    });

    let spec = app.to_servarr_spec(Some(&defaults));
    let p = spec.persistence.unwrap();
    assert_eq!(p.volumes.len(), 1);
    assert_eq!(p.volumes[0].name, "data");
}

#[test]
fn test_merge_persistence_nfs_additive_dedup() {
    let defaults = StackDefaults {
        persistence: Some(PersistenceSpec {
            volumes: Vec::new(),
            nfs_mounts: vec![
                NfsMount {
                    name: "media".into(),
                    server: "192.168.1.100".into(),
                    path: "/exports/media".into(),
                    mount_path: "/media".into(),
                    read_only: false,
                },
                NfsMount {
                    name: "shared".into(),
                    server: "192.168.1.100".into(),
                    path: "/exports/shared".into(),
                    mount_path: "/shared".into(),
                    read_only: true,
                },
            ],
        }),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.persistence = Some(PersistenceSpec {
        volumes: Vec::new(),
        nfs_mounts: vec![NfsMount {
            name: "media".into(),
            server: "10.0.0.1".into(),
            path: "/nfs/media".into(),
            mount_path: "/media".into(),
            read_only: true,
        }],
    });

    let spec = app.to_servarr_spec(Some(&defaults));
    let p = spec.persistence.unwrap();
    assert_eq!(p.nfs_mounts.len(), 2);

    let media = p.nfs_mounts.iter().find(|m| m.name == "media").unwrap();
    assert_eq!(media.server, "10.0.0.1"); // per-app wins
    assert!(media.read_only);

    let shared = p.nfs_mounts.iter().find(|m| m.name == "shared").unwrap();
    assert_eq!(shared.server, "192.168.1.100"); // stack default preserved
}

// ---------------------------------------------------------------------------
// to_servarr_spec: no defaults, with defaults, with overrides
// ---------------------------------------------------------------------------

#[test]
fn test_to_servarr_spec_no_defaults() {
    let mut app = minimal_stack_app(AppType::Radarr);
    app.uid = Some(1000);

    let spec = app.to_servarr_spec(None);
    assert!(matches!(spec.app, AppType::Radarr));
    assert_eq!(spec.uid, Some(1000));
    assert!(spec.gid.is_none());
    assert!(spec.security.is_none());
}

#[test]
fn test_to_servarr_spec_with_defaults() {
    let defaults = StackDefaults {
        uid: Some(568),
        gid: Some(568),
        network_policy: Some(true),
        ..Default::default()
    };

    let app = minimal_stack_app(AppType::Sonarr);
    let spec = app.to_servarr_spec(Some(&defaults));
    assert_eq!(spec.uid, Some(568));
    assert_eq!(spec.gid, Some(568));
    assert_eq!(spec.network_policy, Some(true));
}

#[test]
fn test_to_servarr_spec_with_overrides() {
    let defaults = StackDefaults {
        uid: Some(568),
        gid: Some(568),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.uid = Some(1000);

    let spec = app.to_servarr_spec(Some(&defaults));
    assert_eq!(spec.uid, Some(1000)); // per-app override
    assert_eq!(spec.gid, Some(568)); // stack default
}

// ---------------------------------------------------------------------------
// CRD serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn test_media_stack_serde_roundtrip() {
    let spec = MediaStackSpec {
        nfs: None,
        defaults: Some(StackDefaults {
            uid: Some(568),
            gid: Some(568),
            env: vec![EnvVar {
                name: "TZ".into(),
                value: "UTC".into(),
            }],
            ..Default::default()
        }),
        apps: vec![
            StackApp {
                app: AppType::Jellyfin,
                instance: None,
                enabled: true,
                image: None,
                uid: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                admin_credentials: None,
                split4k: None,
                split4k_overrides: None,
            },
            StackApp {
                app: AppType::Sonarr,
                instance: Some("4k".into()),
                enabled: true,
                uid: Some(1000),
                image: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                admin_credentials: None,
                split4k: None,
                split4k_overrides: None,
            },
        ],
    };

    let json = serde_json::to_string_pretty(&spec).unwrap();
    let deserialized: MediaStackSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.apps.len(), 2);
    assert_eq!(deserialized.apps[0].app, AppType::Jellyfin);
    assert_eq!(deserialized.apps[1].instance.as_deref(), Some("4k"));
    assert_eq!(deserialized.apps[1].uid, Some(1000));
}

// ---------------------------------------------------------------------------
// CRD generation and structural schema validity
// ---------------------------------------------------------------------------

#[test]
fn test_media_stack_crd_generation() {
    use kube::CustomResourceExt;
    let crd = MediaStack::crd();
    let yaml = serde_yaml::to_string(&crd).unwrap();
    assert!(yaml.contains("MediaStack"));
    assert!(yaml.contains("servarr.dev"));
    assert!(yaml.contains("v1alpha1"));
}

#[test]
fn test_media_stack_crd_schema_structural_validity() {
    use kube::CustomResourceExt;

    let crd = MediaStack::crd();
    let json = serde_json::to_value(&crd).unwrap();

    let mut violations = Vec::new();
    check_no_nullable_in_any_of(&json, "$", &mut violations);

    assert!(
        violations.is_empty(),
        "MediaStack CRD schema has Kubernetes structural violations:\n{}",
        violations.join("\n")
    );
}

fn check_no_nullable_in_any_of(
    value: &serde_json::Value,
    path: &str,
    violations: &mut Vec<String>,
) {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return,
    };

    for keyword in ["anyOf", "oneOf"] {
        if let Some(variants) = obj.get(keyword).and_then(|v| v.as_array()) {
            for (i, variant) in variants.iter().enumerate() {
                let variant_path = format!("{path}.{keyword}[{i}]");
                if variant.get("nullable").and_then(|v| v.as_bool()) == Some(true) {
                    violations.push(format!(
                        "{variant_path}: nullable must not appear inside {keyword}"
                    ));
                }
                check_no_nullable_in_any_of(variant, &variant_path, violations);
            }
        }
    }

    if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
        for (key, val) in props {
            check_no_nullable_in_any_of(val, &format!("{path}.properties.{key}"), violations);
        }
    }
    if let Some(items) = obj.get("items") {
        check_no_nullable_in_any_of(items, &format!("{path}.items"), violations);
    }
    if let Some(additional) = obj.get("additionalProperties") {
        check_no_nullable_in_any_of(
            additional,
            &format!("{path}.additionalProperties"),
            violations,
        );
    }

    if let Some(versions) = obj.get("versions").and_then(|v| v.as_array()) {
        for (i, ver) in versions.iter().enumerate() {
            if let Some(schema) = ver.get("schema") {
                check_no_nullable_in_any_of(
                    schema,
                    &format!("{path}.versions[{i}].schema"),
                    violations,
                );
            }
        }
    }
    if let Some(schema) = obj.get("openAPIV3Schema") {
        check_no_nullable_in_any_of(schema, &format!("{path}.openAPIV3Schema"), violations);
    }
}

// ---------------------------------------------------------------------------
// StackPhase display
// ---------------------------------------------------------------------------

#[test]
fn test_stack_phase_display() {
    assert_eq!(StackPhase::Pending.to_string(), "Pending");
    assert_eq!(StackPhase::RollingOut.to_string(), "RollingOut");
    assert_eq!(StackPhase::Ready.to_string(), "Ready");
    assert_eq!(StackPhase::Degraded.to_string(), "Degraded");
}

// ---------------------------------------------------------------------------
// Pod annotations merge
// ---------------------------------------------------------------------------

#[test]
fn test_merge_pod_annotations() {
    let defaults = StackDefaults {
        pod_annotations: Some(std::collections::BTreeMap::from([
            ("prometheus.io/scrape".into(), "true".into()),
            ("example.com/team".into(), "media".into()),
        ])),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.pod_annotations = Some(std::collections::BTreeMap::from([(
        "prometheus.io/scrape".into(),
        "false".into(),
    )]));

    let spec = app.to_servarr_spec(Some(&defaults));
    let annotations = spec.pod_annotations.unwrap();
    assert_eq!(annotations["prometheus.io/scrape"], "false"); // per-app wins
    assert_eq!(annotations["example.com/team"], "media"); // stack default preserved
}

// ---------------------------------------------------------------------------
// split4k: expand()
// ---------------------------------------------------------------------------

#[test]
fn test_expand_no_split4k_produces_one_entry() {
    let app = minimal_stack_app(AppType::Sonarr);
    let result = app.expand("media", "default", None, None).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "media-sonarr");
    assert!(result[0].1.instance.is_none());
}

#[test]
fn test_expand_split4k_false_produces_one_entry() {
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(false);
    let result = app.expand("media", "default", None, None).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "media-sonarr");
}

#[test]
fn test_expand_split4k_true_produces_two_entries() {
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);

    let result = app.expand("media", "default", None, None).unwrap();
    assert_eq!(result.len(), 2);

    // Base instance
    assert_eq!(result[0].0, "media-sonarr");
    assert!(result[0].1.instance.is_none());

    // 4K instance
    assert_eq!(result[1].0, "media-sonarr-4k");
    assert_eq!(result[1].1.instance.as_deref(), Some("4k"));
}

#[test]
fn test_expand_split4k_radarr() {
    let mut app = minimal_stack_app(AppType::Radarr);
    app.split4k = Some(true);

    let result = app.expand("stack", "default", None, None).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].0, "stack-radarr");
    assert_eq!(result[1].0, "stack-radarr-4k");
    assert_eq!(result[1].1.instance.as_deref(), Some("4k"));
}

#[test]
fn test_expand_split4k_invalid_app_type() {
    let mut app = minimal_stack_app(AppType::Prowlarr);
    app.split4k = Some(true);

    let result = app.expand("media", "default", None, None);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("prowlarr"));
}

#[test]
fn test_expand_split4k_invalid_overseerr() {
    let mut app = minimal_stack_app(AppType::Overseerr);
    app.split4k = Some(true);

    let result = app.expand("media", "default", None, None);
    assert!(result.is_err());
}

#[test]
fn test_expand_split4k_overrides_env() {
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);
    app.env = vec![EnvVar {
        name: "TZ".into(),
        value: "UTC".into(),
    }];
    app.split4k_overrides = Some(Split4kOverrides {
        env: vec![EnvVar {
            name: "QUALITY".into(),
            value: "4k".into(),
        }],
        ..Default::default()
    });

    let result = app.expand("media", "default", None, None).unwrap();
    assert_eq!(result.len(), 2);

    // Base should have only TZ
    assert_eq!(result[0].1.env.len(), 1);
    assert_eq!(result[0].1.env[0].name, "TZ");

    // 4K should have both TZ and QUALITY
    assert_eq!(result[1].1.env.len(), 2);
    assert!(result[1].1.env.iter().any(|e| e.name == "TZ"));
    assert!(result[1].1.env.iter().any(|e| e.name == "QUALITY"));
}

#[test]
fn test_expand_split4k_overrides_resources() {
    let mut app = minimal_stack_app(AppType::Radarr);
    app.split4k = Some(true);
    app.split4k_overrides = Some(Split4kOverrides {
        resources: Some(ResourceRequirements {
            limits: ResourceList {
                cpu: "4".into(),
                memory: "2Gi".into(),
            },
            requests: ResourceList {
                cpu: "500m".into(),
                memory: "512Mi".into(),
            },
        }),
        ..Default::default()
    });

    let result = app.expand("media", "default", None, None).unwrap();

    // Base has no resources
    assert!(result[0].1.resources.is_none());

    // 4K has override resources
    let r = result[1].1.resources.as_ref().unwrap();
    assert_eq!(r.limits.cpu, "4");
    assert_eq!(r.limits.memory, "2Gi");
}

#[test]
fn test_split4k_valid_only_sonarr_radarr() {
    assert!(minimal_stack_app(AppType::Sonarr).split4k_valid());
    assert!(minimal_stack_app(AppType::Radarr).split4k_valid());
    assert!(!minimal_stack_app(AppType::Lidarr).split4k_valid());
    assert!(!minimal_stack_app(AppType::Prowlarr).split4k_valid());
    assert!(!minimal_stack_app(AppType::Overseerr).split4k_valid());
    assert!(!minimal_stack_app(AppType::Transmission).split4k_valid());
    assert!(!minimal_stack_app(AppType::Plex).split4k_valid());
}

#[test]
fn test_expand_with_stack_defaults() {
    let defaults = StackDefaults {
        uid: Some(1000),
        gid: Some(1000),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);

    let result = app
        .expand("media", "default", Some(&defaults), None)
        .unwrap();
    assert_eq!(result.len(), 2);

    // Both instances inherit defaults
    assert_eq!(result[0].1.uid, Some(1000));
    assert_eq!(result[1].1.uid, Some(1000));
    assert_eq!(result[1].1.instance.as_deref(), Some("4k"));
}

// ---------------------------------------------------------------------------
// NfsServerSpec — defaults, methods, and serde
// ---------------------------------------------------------------------------

#[test]
fn test_nfs_server_spec_defaults() {
    let nfs = NfsServerSpec::default();
    assert!(nfs.enabled);
    assert_eq!(nfs.storage_size, "1Ti");
    assert!(nfs.storage_class.is_none());
    assert!(nfs.image.is_none());
    assert_eq!(nfs.movies_path, "/movies");
    assert_eq!(nfs.tv_path, "/tv");
    assert_eq!(nfs.music_path, "/music");
    assert_eq!(nfs.movies_4k_path, "/movies-4k");
    assert_eq!(nfs.tv_4k_path, "/tv-4k");
    assert!(nfs.external_server.is_none());
    assert_eq!(nfs.external_path, "/");
}

#[test]
fn test_nfs_server_spec_deploy_in_cluster_default() {
    let nfs = NfsServerSpec::default();
    assert!(nfs.deploy_in_cluster());
}

#[test]
fn test_nfs_server_spec_deploy_in_cluster_disabled() {
    let nfs = NfsServerSpec {
        enabled: false,
        ..Default::default()
    };
    assert!(!nfs.deploy_in_cluster());
}

#[test]
fn test_nfs_server_spec_deploy_in_cluster_external_server_overrides_enabled() {
    let nfs = NfsServerSpec {
        enabled: true,
        external_server: Some("192.168.1.10".to_string()),
        ..Default::default()
    };
    assert!(!nfs.deploy_in_cluster());
}

#[test]
fn test_nfs_server_spec_server_address_in_cluster() {
    let nfs = NfsServerSpec::default();
    assert_eq!(
        nfs.server_address("my-stack", "media"),
        Some("my-stack-nfs-server.media.svc.cluster.local".to_string())
    );
}

#[test]
fn test_nfs_server_spec_server_address_external() {
    let nfs = NfsServerSpec {
        external_server: Some("nas.home.arpa".to_string()),
        ..Default::default()
    };
    assert_eq!(
        nfs.server_address("my-stack", "media"),
        Some("nas.home.arpa".to_string())
    );
}

#[test]
fn test_nfs_server_spec_server_address_disabled() {
    let nfs = NfsServerSpec {
        enabled: false,
        ..Default::default()
    };
    assert_eq!(nfs.server_address("my-stack", "media"), None);
}

#[test]
fn test_nfs_server_spec_nfs_path_in_cluster() {
    // In-cluster server: /nfsshare is the NFSv4 root (fsid=0), so paths are
    // relative to it — no /nfsshare prefix needed from the client's perspective.
    let nfs = NfsServerSpec::default();
    assert_eq!(nfs.nfs_path("/movies"), "/movies");
    assert_eq!(nfs.nfs_path("/tv"), "/tv");
}

#[test]
fn test_nfs_server_spec_nfs_path_external_root() {
    let nfs = NfsServerSpec {
        external_server: Some("nas.home.arpa".to_string()),
        external_path: "/volume1".to_string(),
        ..Default::default()
    };
    assert_eq!(nfs.nfs_path("/data/movies"), "/volume1/data/movies");
    assert_eq!(nfs.nfs_path("/data/tv"), "/volume1/data/tv");
}

#[test]
fn test_nfs_server_spec_nfs_path_external_root_slash() {
    // External server with root-slash external_path is a pass-through.
    let nfs = NfsServerSpec {
        external_server: Some("nas.home.arpa".to_string()),
        external_path: "/".to_string(),
        ..Default::default()
    };
    assert_eq!(nfs.nfs_path("/data/movies"), "/data/movies");
}

#[test]
fn test_nfs_server_spec_custom_paths() {
    let nfs = NfsServerSpec {
        movies_path: "/media/films".to_string(),
        tv_path: "/media/series".to_string(),
        music_path: "/media/audio".to_string(),
        movies_4k_path: "/media/films-uhd".to_string(),
        tv_4k_path: "/media/series-uhd".to_string(),
        ..Default::default()
    };
    assert_eq!(nfs.movies_path, "/media/films");
    assert_eq!(nfs.tv_path, "/media/series");
    assert_eq!(nfs.music_path, "/media/audio");
    assert_eq!(nfs.movies_4k_path, "/media/films-uhd");
    assert_eq!(nfs.tv_4k_path, "/media/series-uhd");
}

#[test]
fn test_nfs_server_spec_serde_roundtrip() {
    let nfs = NfsServerSpec {
        enabled: true,
        storage_size: "2Ti".to_string(),
        storage_class: Some("fast-ssd".to_string()),
        image: None,
        movies_path: "/media/movies".to_string(),
        tv_path: "/media/tv".to_string(),
        music_path: "/media/music".to_string(),
        movies_4k_path: "/media/movies-4k".to_string(),
        tv_4k_path: "/media/tv-4k".to_string(),
        external_server: None,
        external_path: "/".to_string(),
    };
    let json = serde_json::to_string(&nfs).unwrap();
    let decoded: NfsServerSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.storage_size, "2Ti");
    assert_eq!(decoded.storage_class.as_deref(), Some("fast-ssd"));
    assert_eq!(decoded.movies_path, "/media/movies");
}

#[test]
fn test_nfs_server_spec_serde_external() {
    let nfs = NfsServerSpec {
        enabled: false,
        external_server: Some("192.168.1.50".to_string()),
        external_path: "/mnt/data".to_string(),
        ..Default::default()
    };
    let json = serde_json::to_string(&nfs).unwrap();
    let decoded: NfsServerSpec = serde_json::from_str(&json).unwrap();
    assert!(!decoded.enabled);
    assert_eq!(decoded.external_server.as_deref(), Some("192.168.1.50"));
    assert_eq!(decoded.external_path, "/mnt/data");
}

// ---------------------------------------------------------------------------
// NFS mount injection via StackApp::expand
// ---------------------------------------------------------------------------

fn nfs_in_cluster() -> NfsServerSpec {
    NfsServerSpec::default()
}

fn nfs_external() -> NfsServerSpec {
    NfsServerSpec {
        external_server: Some("nas.home.arpa".to_string()),
        external_path: "/volume1".to_string(),
        ..Default::default()
    }
}

#[test]
fn test_nfs_inject_sonarr_gets_tv_mount() {
    let app = minimal_stack_app(AppType::Sonarr);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].name, "tv");
    assert_eq!(
        mounts[0].server,
        "mystack-nfs-server.media.svc.cluster.local"
    );
    assert_eq!(mounts[0].path, "/tv");
    assert_eq!(mounts[0].mount_path, "/tv");
}

#[test]
fn test_nfs_inject_radarr_gets_movies_mount() {
    let app = minimal_stack_app(AppType::Radarr);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].name, "movies");
    assert_eq!(mounts[0].path, "/movies");
    assert_eq!(mounts[0].mount_path, "/movies");
}

#[test]
fn test_nfs_inject_lidarr_gets_music_mount() {
    let app = minimal_stack_app(AppType::Lidarr);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].name, "music");
    assert_eq!(mounts[0].path, "/music");
    assert_eq!(mounts[0].mount_path, "/music");
}

#[test]
fn test_nfs_inject_transmission_gets_all_mounts() {
    let app = minimal_stack_app(AppType::Transmission);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    let names: Vec<&str> = mounts.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"movies"), "expected movies mount");
    assert!(names.contains(&"tv"), "expected tv mount");
    assert!(names.contains(&"music"), "expected music mount");
    assert!(names.contains(&"movies-4k"), "expected movies-4k mount");
    assert!(names.contains(&"tv-4k"), "expected tv-4k mount");
    assert_eq!(mounts.len(), 5);
}

#[test]
fn test_nfs_inject_sabnzbd_gets_all_mounts() {
    let app = minimal_stack_app(AppType::Sabnzbd);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(mounts.len(), 5, "sabnzbd should get all five media mounts");
}

#[test]
fn test_nfs_inject_user_mounts_preserved_by_name() {
    // A user-defined NFS mount with the same name takes precedence.
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.persistence = Some(PersistenceSpec {
        volumes: Vec::new(),
        nfs_mounts: vec![NfsMount {
            name: "tv".to_string(),
            server: "my-custom-server".to_string(),
            path: "/custom/tv".to_string(),
            mount_path: "/tv".to_string(),
            read_only: true,
        }],
    });
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(mounts.len(), 1);
    assert_eq!(
        mounts[0].server, "my-custom-server",
        "user mount should win"
    );
    assert_eq!(mounts[0].path, "/custom/tv");
    assert!(mounts[0].read_only);
}

#[test]
fn test_nfs_inject_external_server_address() {
    let app = minimal_stack_app(AppType::Radarr);
    let nfs = nfs_external();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(mounts[0].server, "nas.home.arpa");
    assert_eq!(mounts[0].path, "/volume1/movies");
}

#[test]
fn test_nfs_inject_disabled_produces_no_mounts() {
    let app = minimal_stack_app(AppType::Sonarr);
    let nfs = NfsServerSpec {
        enabled: false,
        ..Default::default()
    };
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    assert!(
        spec.persistence.is_none() || spec.persistence.as_ref().unwrap().nfs_mounts.is_empty(),
        "disabled NFS should produce no mounts"
    );
}

#[test]
fn test_nfs_inject_split4k_sonarr_uses_4k_server_path_standard_mount() {
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    assert_eq!(result.len(), 2);

    // Standard instance: server path /tv, mounted at /tv
    let (_, std_spec) = &result[0];
    let std_mounts = &std_spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(std_mounts[0].path, "/tv");
    assert_eq!(std_mounts[0].mount_path, "/tv");

    // 4K instance: server path /tv-4k, still mounted at /tv
    let (_, k4_spec) = &result[1];
    let k4_mounts = &k4_spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(k4_mounts[0].path, "/tv-4k");
    assert_eq!(k4_mounts[0].mount_path, "/tv");
}

#[test]
fn test_nfs_inject_split4k_radarr_uses_4k_server_path_standard_mount() {
    let mut app = minimal_stack_app(AppType::Radarr);
    app.split4k = Some(true);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    assert_eq!(result.len(), 2);

    let (_, k4_spec) = &result[1];
    let k4_mounts = &k4_spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(k4_mounts[0].path, "/movies-4k");
    assert_eq!(k4_mounts[0].mount_path, "/movies");
}

#[test]
fn test_nfs_inject_split4k_custom_4k_paths() {
    // Custom tv_4k_path and movies_4k_path must be used for 4K instances.
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);
    let nfs = NfsServerSpec {
        tv_4k_path: "/custom-tv-4k".to_string(),
        ..NfsServerSpec::default()
    };
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, k4_spec) = &result[1];
    let k4_mounts = &k4_spec.persistence.as_ref().unwrap().nfs_mounts;
    assert_eq!(k4_mounts[0].path, "/custom-tv-4k");
    assert_eq!(k4_mounts[0].mount_path, "/tv");
}

#[test]
fn test_nfs_inject_split4k_user_override_via_split4k_overrides() {
    // A user-defined NFS mount in split4k_overrides.persistence takes precedence
    // over the auto-injected 4K mount for the same name.
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);
    app.split4k_overrides = Some(Split4kOverrides {
        persistence: Some(PersistenceSpec {
            volumes: Vec::new(),
            nfs_mounts: vec![NfsMount {
                name: "tv".to_string(),
                server: "custom-override-server".to_string(),
                path: "/override/tv-4k".to_string(),
                mount_path: "/tv".to_string(),
                read_only: false,
            }],
        }),
        ..Default::default()
    });
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, k4_spec) = &result[1];
    let k4_mounts = &k4_spec.persistence.as_ref().unwrap().nfs_mounts;
    let tv_mount = k4_mounts.iter().find(|m| m.name == "tv").unwrap();
    assert_eq!(
        tv_mount.server, "custom-override-server",
        "override should win"
    );
    assert_eq!(tv_mount.path, "/override/tv-4k");
}

#[test]
fn test_nfs_inject_maintainerr_gets_movies_and_tv() {
    let app = minimal_stack_app(AppType::Maintainerr);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    let names: Vec<&str> = mounts.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"movies"), "expected movies mount");
    assert!(names.contains(&"tv"), "expected tv mount");
    assert_eq!(mounts.len(), 2);
    assert_eq!(
        mounts[0].server,
        "mystack-nfs-server.media.svc.cluster.local"
    );
    assert_eq!(mounts[0].path, "/movies");
}

#[test]
fn test_nfs_inject_ssh_bastion_gets_movies_tv_music() {
    let app = minimal_stack_app(AppType::SshBastion);
    let nfs = nfs_in_cluster();
    let result = app.expand("mystack", "media", None, Some(&nfs)).unwrap();
    let (_, spec) = &result[0];
    let mounts = &spec.persistence.as_ref().unwrap().nfs_mounts;
    let names: Vec<&str> = mounts.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"movies"), "expected movies mount");
    assert!(names.contains(&"tv"), "expected tv mount");
    assert!(names.contains(&"music"), "expected music mount");
    assert_eq!(mounts.len(), 3);
}

#[test]
fn test_media_stack_spec_nfs_defaults_to_none() {
    let json = r#"{"apps": [{"app": "Sonarr"}]}"#;
    let spec: MediaStackSpec = serde_json::from_str(json).unwrap();
    assert!(spec.nfs.is_none());
}

#[test]
fn test_media_stack_spec_nfs_field_round_trips() {
    let json = r#"{
        "nfs": { "storageSize": "500Gi", "storageClass": "nfs-fast" },
        "apps": [{"app": "Sonarr"}]
    }"#;
    let spec: MediaStackSpec = serde_json::from_str(json).unwrap();
    let nfs = spec.nfs.unwrap();
    assert!(nfs.enabled);
    assert_eq!(nfs.storage_size, "500Gi");
    assert_eq!(nfs.storage_class.as_deref(), Some("nfs-fast"));
}

// ---------------------------------------------------------------------------
// adminCredentials propagation
// ---------------------------------------------------------------------------

#[test]
fn test_admin_credentials_propagated_from_defaults() {
    let defaults = StackDefaults {
        admin_credentials: Some(AdminCredentialsSpec {
            secret_name: "global-admin".into(),
        }),
        ..Default::default()
    };
    let app = minimal_stack_app(AppType::Sonarr);
    let spec = app.to_servarr_spec(Some(&defaults));
    let ac = spec
        .admin_credentials
        .expect("admin_credentials should be set from defaults");
    assert_eq!(ac.secret_name, "global-admin");
}

#[test]
fn test_admin_credentials_app_overrides_defaults() {
    let defaults = StackDefaults {
        admin_credentials: Some(AdminCredentialsSpec {
            secret_name: "global-admin".into(),
        }),
        ..Default::default()
    };
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "sonarr-admin".into(),
    });
    let spec = app.to_servarr_spec(Some(&defaults));
    let ac = spec
        .admin_credentials
        .expect("admin_credentials should be set");
    assert_eq!(ac.secret_name, "sonarr-admin");
}

#[test]
fn test_admin_credentials_none_when_unset() {
    let app = minimal_stack_app(AppType::Radarr);
    let spec = app.to_servarr_spec(None);
    assert!(spec.admin_credentials.is_none());
}

#[test]
fn test_admin_credentials_split4k_override() {
    let defaults = StackDefaults {
        admin_credentials: Some(AdminCredentialsSpec {
            secret_name: "global-admin".into(),
        }),
        ..Default::default()
    };
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);
    app.split4k_overrides = Some(Split4kOverrides {
        admin_credentials: Some(AdminCredentialsSpec {
            secret_name: "4k-admin".into(),
        }),
        ..Default::default()
    });

    let pairs = app
        .expand("test-stack", "test-ns", Some(&defaults), None)
        .expect("expand should succeed");

    // Standard instance uses global-admin from defaults
    let std_pair = pairs
        .iter()
        .find(|(name, _spec)| !name.contains("4k"))
        .unwrap();
    assert_eq!(
        std_pair.1.admin_credentials.as_ref().unwrap().secret_name,
        "global-admin"
    );

    // 4K instance uses the override
    let k4_pair = pairs
        .iter()
        .find(|(name, _spec)| name.contains("4k"))
        .unwrap();
    assert_eq!(
        k4_pair.1.admin_credentials.as_ref().unwrap().secret_name,
        "4k-admin"
    );
}

#[test]
fn test_admin_credentials_serde_roundtrip() {
    let ac = AdminCredentialsSpec {
        secret_name: "my-admin-secret".into(),
    };
    let json = serde_json::to_string(&ac).unwrap();
    let deserialized: AdminCredentialsSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.secret_name, "my-admin-secret");
}
