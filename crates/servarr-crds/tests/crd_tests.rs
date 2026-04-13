use servarr_crds::*;

#[test]
fn test_crd_serde_roundtrip_sonarr() {
    let spec = ServarrAppSpec {
        app: AppType::Sonarr,
        ..Default::default()
    };

    let json = serde_json::to_string(&spec).unwrap();
    let deserialized: ServarrAppSpec = serde_json::from_str(&json).unwrap();
    assert!(matches!(deserialized.app, AppType::Sonarr));
}

#[test]
fn test_crd_serde_roundtrip_transmission_with_config() {
    let spec = ServarrAppSpec {
        app: AppType::Transmission,
        app_config: Some(AppConfig::Transmission(TransmissionConfig {
            settings: serde_json::json!({
                "download-dir": "/data/complete",
                "encryption": 2,
            }),
            peer_port: Some(PeerPortConfig {
                port: 51413,
                host_port: true,
                random_on_start: false,
                random_low: 49152,
                random_high: 65535,
            }),
            auth: Some(TransmissionAuth {
                secret_name: "transmission-auth".into(),
            }),
        })),
        ..Default::default()
    };

    let json = serde_json::to_string(&spec).unwrap();
    let deserialized: ServarrAppSpec = serde_json::from_str(&json).unwrap();
    assert!(matches!(deserialized.app, AppType::Transmission));
    assert!(deserialized.app_config.is_some());
}

#[test]
fn test_crd_serde_roundtrip_all_fields() {
    let spec = ServarrAppSpec {
        app: AppType::Radarr,
        instance: Some("4k".into()),
        image: Some(ImageSpec {
            repository: "linuxserver/radarr".into(),
            tag: "5.0.0".into(),
            digest: String::new(),
            pull_policy: "Always".into(),
        }),
        uid: Some(1000),
        gid: Some(1000),
        security: Some(SecurityProfile::linux_server(1000, 1000)),
        service: Some(ServiceSpec {
            service_type: "ClusterIP".into(),
            ports: vec![ServicePort {
                name: "http".into(),
                port: 7878,
                protocol: "TCP".into(),
                container_port: None,
                host_port: None,
            }],
        }),
        gateway: Some(GatewaySpec {
            enabled: true,
            route_type: RouteType::Http,
            parent_refs: vec![GatewayParentRef {
                name: "my-gateway".into(),
                namespace: "istio-system".into(),
                section_name: String::new(),
            }],
            hosts: vec!["radarr.example.com".into()],
            tls: None,
        }),
        resources: Some(ResourceRequirements {
            limits: ResourceList {
                cpu: "2".into(),
                memory: "1Gi".into(),
            },
            requests: ResourceList {
                cpu: "200m".into(),
                memory: "256Mi".into(),
            },
        }),
        persistence: Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "config".into(),
                mount_path: "/config".into(),
                access_mode: "ReadWriteOnce".into(),
                size: "2Gi".into(),
                storage_class: "fast".into(),
            }],
            nfs_mounts: vec![NfsMount {
                name: "media".into(),
                server: "192.168.1.100".into(),
                path: "/exports/media".into(),
                mount_path: "/media".into(),
                read_only: false,
            }],
        }),
        env: vec![EnvVar {
            name: "TZ".into(),
            value: "America/New_York".into(),
        }],
        probes: Some(ProbeSpec {
            liveness: ProbeConfig::default(),
            readiness: ProbeConfig::default(),
        }),
        scheduling: None,
        network_policy: Some(true),
        network_policy_config: None,
        app_config: None,
        api_key_secret: Some("radarr-api-key".into()),
        api_health_check: None,
        backup: None,
        image_pull_secrets: Some(vec!["ghcr-secret".into()]),
        pod_annotations: Some(std::collections::BTreeMap::from([(
            "prometheus.io/scrape".into(),
            "true".into(),
        )])),
        gpu: None,
        prowlarr_sync: None,
        overseerr_sync: None,
        admin_credentials: None,
    };

    let json = serde_json::to_string_pretty(&spec).unwrap();
    let deserialized: ServarrAppSpec = serde_json::from_str(&json).unwrap();
    assert!(matches!(deserialized.app, AppType::Radarr));
    assert_eq!(deserialized.uid, Some(1000));
    assert_eq!(deserialized.env.len(), 1);
    assert!(deserialized.persistence.is_some());
    let p = deserialized.persistence.unwrap();
    assert_eq!(p.volumes.len(), 1);
    assert_eq!(p.nfs_mounts.len(), 1);
}

#[test]
fn test_defaults_for_all_app_types() {
    let app_types = vec![
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
    ];

    for app_type in &app_types {
        let defaults = AppDefaults::for_app(app_type);
        assert!(
            !defaults.image.repository.is_empty(),
            "{app_type}: empty image repo"
        );
        assert!(
            !defaults.image.tag.is_empty(),
            "{app_type}: empty image tag"
        );
        assert!(
            !defaults.service.ports.is_empty(),
            "{app_type}: no service ports"
        );
        assert!(
            !defaults.persistence.volumes.is_empty(),
            "{app_type}: no volumes"
        );
    }
}

#[test]
fn test_linuxserver_apps_have_downloads_pvc() {
    let with_downloads = vec![
        AppType::Sonarr,
        AppType::Radarr,
        AppType::Lidarr,
        AppType::Sabnzbd,
        AppType::Transmission,
    ];

    for app_type in &with_downloads {
        let defaults = AppDefaults::for_app(app_type);
        let has_downloads = defaults
            .persistence
            .volumes
            .iter()
            .any(|v| v.name == "downloads");
        assert!(has_downloads, "{app_type} should have downloads PVC");
    }
}

#[test]
fn test_config_only_apps() {
    let config_only = vec![
        AppType::Prowlarr,
        AppType::Tautulli,
        AppType::Overseerr,
        AppType::Jackett,
        AppType::Maintainerr,
        AppType::Jellyfin,
        AppType::Plex,
    ];

    for app_type in &config_only {
        let defaults = AppDefaults::for_app(app_type);
        assert_eq!(
            defaults.persistence.volumes.len(),
            1,
            "{app_type} should have exactly 1 volume"
        );
        assert_eq!(defaults.persistence.volumes[0].name, "config");
    }
}

#[test]
fn test_maintainerr_is_nonroot() {
    let defaults = AppDefaults::for_app(&AppType::Maintainerr);
    assert!(matches!(
        defaults.security.profile_type,
        SecurityProfileType::NonRoot
    ));
}

#[test]
fn test_transmission_has_app_config() {
    let defaults = AppDefaults::for_app(&AppType::Transmission);
    assert!(matches!(
        defaults.app_config,
        Some(AppConfig::Transmission(_))
    ));
}

#[test]
fn test_app_type_display() {
    assert_eq!(AppType::Sonarr.to_string(), "sonarr");
    assert_eq!(AppType::Radarr.to_string(), "radarr");
    assert_eq!(AppType::Transmission.to_string(), "transmission");
    assert_eq!(AppType::Maintainerr.to_string(), "maintainerr");
    assert_eq!(AppType::Jellyfin.to_string(), "jellyfin");
    assert_eq!(AppType::Plex.to_string(), "plex");
}

#[test]
fn test_crd_generation() {
    use kube::CustomResourceExt;
    let crd = ServarrApp::crd();
    let yaml = serde_yaml::to_string(&crd).unwrap();
    assert!(yaml.contains("ServarrApp"));
    assert!(yaml.contains("servarr.dev"));
    assert!(yaml.contains("v1alpha1"));
}

/// Validate that the generated CRD schema is compatible with Kubernetes
/// structural schema requirements.
///
/// Kubernetes rejects CRDs where `nullable: true` appears inside `anyOf`
/// or `oneOf` blocks. This test catches schema regressions that would only
/// surface during smoke tests on a real cluster.
#[test]
fn test_crd_schema_structural_validity() {
    use kube::CustomResourceExt;

    let crd = ServarrApp::crd();
    let json = serde_json::to_value(&crd).unwrap();

    // Walk the entire schema tree looking for structural violations
    let mut violations = Vec::new();
    check_no_nullable_in_any_of(&json, "$", &mut violations);

    assert!(
        violations.is_empty(),
        "CRD schema has Kubernetes structural violations:\n{}",
        violations.join("\n")
    );
}

/// Recursively check that no `nullable: true` appears inside `anyOf` or `oneOf` items.
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

    // Recurse into properties, items, additionalProperties
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

    // Recurse into spec versions
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

/// Validate that the CI smoke-test manifests deserialise cleanly against the
/// current ServarrApp CRD.  Catches field renames / removals before they reach
/// the kind cluster.
#[test]
fn test_smoke_test_manifests_match_crd() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .parent() // repo root
        .unwrap()
        .join(".github/smoke-test/manifests");

    assert!(
        manifest_dir.is_dir(),
        "smoke-test manifests dir missing: {}",
        manifest_dir.display()
    );

    let mut count = 0;
    for entry in std::fs::read_dir(&manifest_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        // Parse as a full Kubernetes-style object with apiVersion/kind/metadata/spec
        let doc: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap_or_else(|e| {
            panic!("{}: invalid YAML: {e}", path.display());
        });
        let kind = doc
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("ServarrApp");
        // Skip non-CRD Kubernetes resources (e.g. Secrets) — they have no spec to validate.
        if matches!(kind, "Secret" | "ConfigMap" | "ServiceAccount") {
            continue;
        }
        let spec = doc
            .get("spec")
            .unwrap_or_else(|| panic!("{}: missing 'spec' key", path.display()));
        // Strict deserialise: unknown fields will fail via deny_unknown_fields
        // serde_yaml → serde_json → the appropriate spec type.
        let spec_json = serde_json::to_value(spec).unwrap();
        match kind {
            "MediaStack" => {
                let result: Result<MediaStackSpec, _> = serde_json::from_value(spec_json);
                assert!(
                    result.is_ok(),
                    "{}: spec does not match MediaStackSpec: {}",
                    path.display(),
                    result.unwrap_err()
                );
            }
            _ => {
                let result: Result<ServarrAppSpec, _> = serde_json::from_value(spec_json);
                assert!(
                    result.is_ok(),
                    "{}: spec does not match ServarrAppSpec: {}",
                    path.display(),
                    result.unwrap_err()
                );
            }
        }
        count += 1;
    }
    assert!(
        count >= 14,
        "expected at least 14 smoke-test manifests, found {count}"
    );
}

#[test]
fn test_status_serde() {
    let status = ServarrAppStatus {
        ready: true,
        ready_replicas: 1,
        observed_generation: 5,
        conditions: vec![Condition {
            condition_type: "Ready".into(),
            status: "True".into(),
            reason: "DeploymentReady".into(),
            message: "1 replica(s) ready".into(),
            last_transition_time: "2024-01-01T00:00:00Z".into(),
        }],
        backup_status: None,
    };

    let json = serde_json::to_string(&status).unwrap();
    let deserialized: ServarrAppStatus = serde_json::from_str(&json).unwrap();
    assert!(deserialized.ready);
    assert_eq!(deserialized.ready_replicas, 1);
    assert_eq!(deserialized.conditions.len(), 1);
}
