use serde::Deserialize;
use servarr_crds::*;

/// Regression test for #540: schemars does not propagate serde `#[serde(alias = ...)]`
/// annotations into the generated JSON Schema, so the CRD's `app` enum silently dropped
/// `Overseerr` even though `AppType` still deserializes it. That makes a fresh `kubectl
/// apply` of a pre-1.3 manifest fail the CRD's structural-schema validation, even though
/// already-stored objects with `app: Overseerr` keep reconciling fine.
#[test]
fn app_type_json_schema_enum_includes_legacy_overseerr_alias() {
    let schema = schemars::schema_for!(AppType);
    let enum_values: Vec<&str> = schema
        .get("enum")
        .and_then(|v| v.as_array())
        .expect("AppType schema should have an enum array")
        .iter()
        .map(|v| v.as_str().expect("enum values should be strings"))
        .collect();

    assert!(
        enum_values.contains(&"Overseerr"),
        "schema enum {enum_values:?} must include the legacy 'Overseerr' alias so a \
         kubectl apply of a pre-1.3 manifest isn't rejected outright (#540)"
    );

    // Every current variant's wire name must still be present -- the schema must not
    // just be a hardcoded legacy list, it must still track AppType::ALL.
    for app in AppType::ALL {
        let wire_name = serde_json::to_value(app)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            enum_values.contains(&wire_name.as_str()),
            "schema enum {enum_values:?} is missing current variant '{wire_name}'"
        );
    }
}

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
        service_name: Some("radarr4k".into()),
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
            enabled: Some(true),
            route_type: Some(RouteType::Http),
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
                existing_claim_name: None,
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
        seerr_sync: None,
        bazarr_sync: None,
        subgen_sync: None,
        maintainerr_sync: None,
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
        AppType::Seerr,
        AppType::Maintainerr,
        AppType::Jackett,
        AppType::Jellyfin,
        AppType::Plex,
    ];

    for app_type in &app_types {
        let defaults = AppDefaults::for_app(app_type).unwrap();
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
        let defaults = AppDefaults::for_app(app_type).unwrap();
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
        AppType::Seerr,
        AppType::Jackett,
        AppType::Maintainerr,
        AppType::Jellyfin,
        AppType::Plex,
    ];

    for app_type in &config_only {
        let defaults = AppDefaults::for_app(app_type).unwrap();
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
    let defaults = AppDefaults::for_app(&AppType::Maintainerr).unwrap();
    assert!(matches!(
        defaults.security.profile_type,
        SecurityProfileType::NonRoot
    ));
}

#[test]
fn test_transmission_has_app_config() {
    let defaults = AppDefaults::for_app(&AppType::Transmission).unwrap();
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

#[test]
fn test_service_name_has_validation_constraints() {
    use kube::CustomResourceExt;

    let crd = ServarrApp::crd();
    let json = serde_json::to_value(&crd).unwrap();

    // Navigate to the serviceName field in the schema
    let service_name = json
        .pointer("/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/serviceName")
        .expect("serviceName not found in CRD schema");

    assert_eq!(
        service_name.get("type").and_then(|v| v.as_str()),
        Some("string"),
        "serviceName should be a string type"
    );
    assert_eq!(
        service_name.get("pattern").and_then(|v| v.as_str()),
        Some("^[a-z0-9]([a-z0-9-]*[a-z0-9])?$"),
        "serviceName should have RFC 1123 label pattern"
    );
    assert_eq!(
        service_name.get("maxLength").and_then(|v| v.as_u64()),
        Some(63),
        "serviceName should have maxLength 63"
    );
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

/// Same intent as [`test_smoke_test_manifests_match_crd`], for `docs/examples/*.yaml`.
///
/// These docs examples had no schema validation at all before this test: issue #44's rename
/// caught `docs/examples/overseerr.yaml`'s `appConfig` key spelled `Overseerr` (capitalized),
/// which never actually matched the `#[serde(rename_all = "camelCase")]` wire form (lowercase
/// `overseerr`) and would have failed to apply. Multi-document files (`---`-separated) are
/// supported since several examples show more than one variant in one file.
#[test]
fn test_docs_examples_match_crd() {
    let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .parent() // repo root
        .unwrap()
        .join("docs/examples");

    assert!(
        examples_dir.is_dir(),
        "docs examples dir missing: {}",
        examples_dir.display()
    );

    let mut count = 0;
    for entry in std::fs::read_dir(&examples_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        for doc in serde_yaml::Deserializer::from_str(&contents) {
            let doc = serde_yaml::Value::deserialize(doc).unwrap_or_else(|e| {
                panic!("{}: invalid YAML document: {e}", path.display());
            });
            let kind = doc
                .get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or("ServarrApp");
            if matches!(kind, "Secret" | "ConfigMap" | "ServiceAccount") {
                continue;
            }
            let Some(spec) = doc.get("spec") else {
                continue;
            };
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
    }
    assert!(
        count >= 16,
        "expected at least 16 docs example documents, found {count}"
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

#[test]
fn app_type_deserializes_legacy_overseerr_spelling() {
    let parsed: AppType = serde_json::from_str("\"Overseerr\"").unwrap();
    assert_eq!(parsed, AppType::Seerr);
}

#[test]
fn app_type_serializes_as_seerr() {
    let json = serde_json::to_string(&AppType::Seerr).unwrap();
    assert_eq!(json, "\"Seerr\"");
}

#[test]
fn seerr_sync_field_deserializes_legacy_overseerr_sync_spelling() {
    let spec = ServarrAppSpec {
        app: AppType::Seerr,
        seerr_sync: Some(SeerrSyncSpec {
            enabled: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let json = serde_json::to_string(&spec).unwrap();
    // Simulate a CR persisted before the rename: same JSON, but with the old field name.
    let legacy_json = json.replace("\"seerrSync\"", "\"overseerrSync\"");
    assert!(
        legacy_json.contains("overseerrSync"),
        "test setup: legacy_json should contain the old field name"
    );
    let deserialized: ServarrAppSpec = serde_json::from_str(&legacy_json).unwrap();
    assert!(deserialized.seerr_sync.is_some());
    assert!(deserialized.seerr_sync.unwrap().enabled);
}

/// Regression test for #545: unlike `AppType`'s enum alias (#540), `seerr_sync`'s
/// `#[serde(alias = "overseerrSync")]` is on an object-typed field, so schemars can't fold it
/// into the generated schema the same way -- the CRD's structural schema only declared a
/// `seerrSync` property, which silently pruned a manifest still using `overseerrSync` instead of
/// rejecting it. Assert the generated `ServarrApp` CRD schema exposes both property names.
#[test]
fn servarr_app_crd_schema_includes_legacy_overseerr_sync_alias() {
    let crd = crd_with_legacy_field_aliases::<ServarrApp>();
    let props = crd
        .spec
        .versions
        .iter()
        .find(|v| v.name == "v1alpha1")
        .and_then(|v| v.schema.as_ref())
        .and_then(|s| s.open_api_v3_schema.as_ref())
        .and_then(|s| s.properties.as_ref())
        .and_then(|p| p.get("spec"))
        .and_then(|spec_schema| spec_schema.properties.as_ref())
        .expect("ServarrApp CRD schema should have spec.properties");

    assert!(
        props.contains_key("seerrSync"),
        "sanity check: seerrSync should be present in the generated schema"
    );
    assert!(
        props.contains_key("overseerrSync"),
        "CRD schema must also expose overseerrSync so a kubectl apply of a manifest using the \
         legacy field name isn't silently pruned by structural-schema validation (#545)"
    );
}

/// Same regression as above, for `MediaStack` -- `seerr_sync` lives on `StackApp`
/// (media_stack.rs), nested inside `spec.apps[]`, not directly on `MediaStackSpec`. Needs the
/// same schema fix, at a different nesting depth.
#[test]
fn media_stack_crd_schema_includes_legacy_overseerr_sync_alias() {
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::JSONSchemaPropsOrArray;

    let crd = crd_with_legacy_field_aliases::<MediaStack>();
    let props = crd
        .spec
        .versions
        .iter()
        .find(|v| v.name == "v1alpha1")
        .and_then(|v| v.schema.as_ref())
        .and_then(|s| s.open_api_v3_schema.as_ref())
        .and_then(|s| s.properties.as_ref())
        .and_then(|p| p.get("spec"))
        .and_then(|spec_schema| spec_schema.properties.as_ref())
        .and_then(|p| p.get("apps"))
        .and_then(|apps_schema| apps_schema.items.as_ref())
        .and_then(|items| match items {
            JSONSchemaPropsOrArray::Schema(s) => Some(s.as_ref()),
            JSONSchemaPropsOrArray::Schemas(_) => None,
        })
        .and_then(|item_schema| item_schema.properties.as_ref())
        .expect("MediaStack CRD schema should have spec.apps.items.properties");

    assert!(
        props.contains_key("seerrSync"),
        "sanity check: seerrSync should be present in the generated schema"
    );
    assert!(
        props.contains_key("overseerrSync"),
        "CRD schema must also expose overseerrSync so a kubectl apply of a manifest using the \
         legacy field name isn't silently pruned by structural-schema validation (#545)"
    );
}

// ── Property test: AppConfig::Seerr legacy "overseerr" discriminator ──
//
// The AppConfig enum renames the Overseerr variant to Seerr with
// #[serde(alias = "overseerr")]. For any generated Seerr config, feeding the
// old discriminator through deserialization must reproduce the new one.

mod app_config_seerr_alias {
    use proptest::prelude::*;
    use servarr_crds::*;

    /// Quality profile IDs are small integers, so generate an f64 from an i32:
    /// NaN/Inf would serialize as JSON null and not round-trip, and serde_json's
    /// float parser is off-by-one-ULP for some values above 2^52 (reproduced
    /// with 7.37e15). i32 magnitudes round-trip exactly — keep it deterministic.
    fn finite_f64() -> impl Strategy<Value = f64> {
        any::<i32>().prop_map(|v| v as f64)
    }

    fn seerr_server_defaults_4k() -> impl Strategy<Value = SeerrServerDefaults4k> {
        (
            finite_f64(),
            any::<String>(),
            any::<String>(),
            proptest::option::of(any::<String>()),
            proptest::option::of(any::<bool>()),
        )
            .prop_map(
                |(
                    profile_id,
                    profile_name,
                    root_folder,
                    minimum_availability,
                    enable_season_folders,
                )| SeerrServerDefaults4k {
                    profile_id,
                    profile_name,
                    root_folder,
                    minimum_availability,
                    enable_season_folders,
                },
            )
    }

    fn seerr_server_defaults() -> impl Strategy<Value = SeerrServerDefaults> {
        (
            finite_f64(),
            any::<String>(),
            any::<String>(),
            proptest::option::of(any::<String>()),
            proptest::option::of(any::<bool>()),
            proptest::option::of(seerr_server_defaults_4k()),
        )
            .prop_map(
                |(
                    profile_id,
                    profile_name,
                    root_folder,
                    minimum_availability,
                    enable_season_folders,
                    four_k,
                )| SeerrServerDefaults {
                    profile_id,
                    profile_name,
                    root_folder,
                    minimum_availability,
                    enable_season_folders,
                    four_k,
                },
            )
    }

    fn seerr_config() -> impl Strategy<Value = SeerrConfig> {
        (
            proptest::option::of(seerr_server_defaults()),
            proptest::option::of(seerr_server_defaults()),
        )
            .prop_map(|(sonarr, radarr)| SeerrConfig { sonarr, radarr })
    }

    proptest! {
        #[test]
        fn seerr_app_config_deserializes_legacy_overseerr_discriminator(config in seerr_config()) {
            let canonical = AppConfig::Seerr(Box::new(config));
            let json = serde_json::to_string(&canonical).unwrap();
            // Simulate a CR persisted before the rename: the old "overseerr" discriminator.
            let legacy_json = json.replace("\"Seerr\"", "\"overseerr\"");
            let parsed: AppConfig = serde_json::from_str(&legacy_json).unwrap();
            let reserialized = serde_json::to_string(&parsed).unwrap();
            prop_assert_eq!(
                reserialized, json,
                "deserializing the legacy overseerr discriminator must reproduce the Seerr form"
            );
        }
    }
}
