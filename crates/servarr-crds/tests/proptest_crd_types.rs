//! Property-based serde coverage for CRD types beyond the status types already
//! covered in `proptest_serde.rs` (#34).
//!
//! Three properties are exercised:
//! - Roundtrip: re-serializing a deserialized value yields the same JSON.
//! - camelCase stability: field renames hold for every generated value.
//! - Default injection: missing fields deserialize to documented defaults.
//!
//! Roundtrip is asserted on the JSON `Value` rather than the Rust value so the
//! check works for types that do not derive `PartialEq`.

use proptest::prelude::*;
use servarr_crds::*;

/// Re-serialize a deserialized value and assert the JSON is unchanged.
fn assert_roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) {
    let json = serde_json::to_string(value).expect("serialization failed");
    let back: T = serde_json::from_str(&json).expect("deserialization failed");
    assert_eq!(
        serde_json::to_value(value).expect("orig to_value failed"),
        serde_json::to_value(&back).expect("roundtrip to_value failed"),
        "JSON representation changed across roundtrip"
    );
}

fn arb_string() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ._/:-]{0,30}".prop_map(String::from)
}

fn arb_opt_string() -> impl Strategy<Value = Option<String>> {
    prop_oneof![Just(None), arb_string().prop_map(Some)]
}

fn arb_opt_bool() -> impl Strategy<Value = Option<bool>> {
    prop::option::of(any::<bool>())
}

fn arb_opt_i32() -> impl Strategy<Value = Option<i32>> {
    prop::option::of(any::<i32>())
}

/// A small, valid JSON object — used for the opaque `serde_json::Value` fields
/// (tolerations, affinity, custom egress rules) so roundtrip stays meaningful
/// without generating pathological JSON.
fn arb_json_object() -> impl Strategy<Value = serde_json::Value> {
    (arb_string(), arb_string()).prop_map(|(k, v)| serde_json::json!({ k: v }))
}

fn arb_json_objects() -> impl Strategy<Value = Vec<serde_json::Value>> {
    prop::collection::vec(arb_json_object(), 0..3)
}

prop_compose! {
    // Named bindings (vs a 4-tuple of identical `arb_string()`) so a field
    // can't be silently swapped — this strategy backs the #38 merge tests.
    fn arb_image()(
        repository in arb_string(),
        tag in arb_string(),
        digest in arb_string(),
        pull_policy in arb_string(),
    ) -> ImageSpec {
        ImageSpec { repository, tag, digest, pull_policy }
    }
}

fn arb_pvc() -> impl Strategy<Value = PvcVolume> {
    (
        arb_string(),
        arb_string(),
        arb_string(),
        arb_string(),
        arb_string(),
        arb_opt_string(),
    )
        .prop_map(
            |(name, mount_path, access_mode, size, storage_class, existing_claim_name)| PvcVolume {
                name,
                mount_path,
                access_mode,
                size,
                storage_class,
                existing_claim_name,
            },
        )
}

fn arb_nfs_mount() -> impl Strategy<Value = NfsMount> {
    (
        arb_string(),
        arb_string(),
        arb_string(),
        arb_string(),
        any::<bool>(),
    )
        .prop_map(|(name, server, path, mount_path, read_only)| NfsMount {
            name,
            server,
            path,
            mount_path,
            read_only,
        })
}

fn arb_persistence() -> impl Strategy<Value = PersistenceSpec> {
    (
        prop::collection::vec(arb_pvc(), 0..4),
        prop::collection::vec(arb_nfs_mount(), 0..4),
    )
        .prop_map(|(volumes, nfs_mounts)| PersistenceSpec {
            volumes,
            nfs_mounts,
        })
}

fn arb_service_port() -> impl Strategy<Value = ServicePort> {
    (
        arb_string(),
        any::<i32>(),
        arb_string(),
        arb_opt_i32(),
        arb_opt_i32(),
    )
        .prop_map(
            |(name, port, protocol, container_port, host_port)| ServicePort {
                name,
                port,
                protocol,
                container_port,
                host_port,
            },
        )
}

fn arb_service() -> impl Strategy<Value = ServiceSpec> {
    (
        arb_string(),
        prop::collection::vec(arb_service_port(), 0..4),
    )
        .prop_map(|(service_type, ports)| ServiceSpec {
            service_type,
            ports,
        })
}

fn arb_security_profile_type() -> impl Strategy<Value = SecurityProfileType> {
    prop_oneof![
        Just(SecurityProfileType::LinuxServer),
        Just(SecurityProfileType::NonRoot),
        Just(SecurityProfileType::Custom),
    ]
}

fn arb_security_profile() -> impl Strategy<Value = SecurityProfile> {
    (
        arb_security_profile_type(),
        any::<i64>(),
        any::<i64>(),
        arb_opt_bool(),
        arb_opt_bool(),
        arb_opt_bool(),
        prop::collection::vec(arb_string(), 0..3),
        prop::collection::vec(arb_string(), 0..3),
    )
        .prop_map(
            |(
                profile_type,
                user,
                group,
                run_as_non_root,
                read_only_root_filesystem,
                allow_privilege_escalation,
                capabilities_add,
                capabilities_drop,
            )| SecurityProfile {
                profile_type,
                user,
                group,
                run_as_non_root,
                read_only_root_filesystem,
                allow_privilege_escalation,
                capabilities_add,
                capabilities_drop,
            },
        )
}

fn arb_resource_list() -> impl Strategy<Value = ResourceList> {
    (arb_string(), arb_string()).prop_map(|(cpu, memory)| ResourceList { cpu, memory })
}

fn arb_resources() -> impl Strategy<Value = ResourceRequirements> {
    (arb_resource_list(), arb_resource_list())
        .prop_map(|(limits, requests)| ResourceRequirements { limits, requests })
}

fn arb_route_type() -> impl Strategy<Value = RouteType> {
    prop_oneof![Just(RouteType::Http), Just(RouteType::Tcp)]
}

fn arb_parent_ref() -> impl Strategy<Value = GatewayParentRef> {
    (arb_string(), arb_string(), arb_string()).prop_map(|(name, namespace, section_name)| {
        GatewayParentRef {
            name,
            namespace,
            section_name,
        }
    })
}

fn arb_tls() -> impl Strategy<Value = TlsSpec> {
    (any::<bool>(), arb_string(), arb_opt_string()).prop_map(
        |(enabled, cert_issuer, secret_name)| TlsSpec {
            enabled,
            cert_issuer,
            secret_name,
        },
    )
}

fn arb_gateway() -> impl Strategy<Value = GatewaySpec> {
    (
        arb_opt_bool(),
        prop_oneof![Just(None), arb_route_type().prop_map(Some)],
        prop::collection::vec(arb_parent_ref(), 0..3),
        prop::collection::vec(arb_string(), 0..3),
        prop_oneof![Just(None), arb_tls().prop_map(Some)],
    )
        .prop_map(
            |(enabled, route_type, parent_refs, hosts, tls)| GatewaySpec {
                enabled,
                route_type,
                parent_refs,
                hosts,
                tls,
            },
        )
}

fn arb_env_var() -> impl Strategy<Value = EnvVar> {
    (arb_string(), arb_string()).prop_map(|(name, value)| EnvVar { name, value })
}

fn arb_backup() -> impl Strategy<Value = BackupSpec> {
    (any::<bool>(), arb_string(), any::<u32>()).prop_map(|(enabled, schedule, retention_count)| {
        BackupSpec {
            enabled,
            schedule,
            retention_count,
        }
    })
}

fn arb_gpu() -> impl Strategy<Value = GpuSpec> {
    (arb_opt_i32(), arb_opt_i32(), arb_opt_i32()).prop_map(|(nvidia, intel, amd)| GpuSpec {
        nvidia,
        intel,
        amd,
    })
}

fn arb_network_policy() -> impl Strategy<Value = NetworkPolicyConfig> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        prop::collection::vec(arb_string(), 0..3),
        arb_json_objects(),
    )
        .prop_map(
            |(
                allow_same_namespace,
                allow_dns,
                allow_internet_egress,
                denied_cidr_blocks,
                custom_egress_rules,
            )| NetworkPolicyConfig {
                allow_same_namespace,
                allow_dns,
                allow_internet_egress,
                denied_cidr_blocks,
                custom_egress_rules,
            },
        )
}

fn arb_node_scheduling() -> impl Strategy<Value = NodeScheduling> {
    (
        prop::collection::btree_map(arb_string(), arb_string(), 0..3),
        arb_json_objects(),
        prop_oneof![Just(None), arb_json_object().prop_map(Some)],
    )
        .prop_map(|(node_selector, tolerations, affinity)| NodeScheduling {
            node_selector,
            tolerations,
            affinity,
        })
}

fn arb_probe_type() -> impl Strategy<Value = ProbeType> {
    prop_oneof![
        Just(ProbeType::Http),
        Just(ProbeType::Tcp),
        Just(ProbeType::Exec),
    ]
}

prop_compose! {
    fn arb_probe_config()(
        probe_type in arb_probe_type(),
        path in arb_string(),
        command in prop::collection::vec(arb_string(), 0..3),
        initial_delay_seconds in any::<i32>(),
        period_seconds in any::<i32>(),
        timeout_seconds in any::<i32>(),
        failure_threshold in any::<i32>(),
    ) -> ProbeConfig {
        ProbeConfig {
            probe_type,
            path,
            command,
            initial_delay_seconds,
            period_seconds,
            timeout_seconds,
            failure_threshold,
        }
    }
}

prop_compose! {
    fn arb_probe_spec()(
        liveness in arb_probe_config(),
        readiness in arb_probe_config(),
    ) -> ProbeSpec {
        ProbeSpec { liveness, readiness }
    }
}

proptest! {
    #[test]
    fn prop_image_roundtrip(v in arb_image()) { assert_roundtrip(&v); }

    #[test]
    fn prop_pvc_roundtrip(v in arb_pvc()) { assert_roundtrip(&v); }

    #[test]
    fn prop_nfs_mount_roundtrip(v in arb_nfs_mount()) { assert_roundtrip(&v); }

    #[test]
    fn prop_persistence_roundtrip(v in arb_persistence()) { assert_roundtrip(&v); }

    #[test]
    fn prop_service_port_roundtrip(v in arb_service_port()) { assert_roundtrip(&v); }

    #[test]
    fn prop_service_roundtrip(v in arb_service()) { assert_roundtrip(&v); }

    #[test]
    fn prop_security_profile_roundtrip(v in arb_security_profile()) { assert_roundtrip(&v); }

    #[test]
    fn prop_resources_roundtrip(v in arb_resources()) { assert_roundtrip(&v); }

    #[test]
    fn prop_gateway_roundtrip(v in arb_gateway()) { assert_roundtrip(&v); }

    #[test]
    fn prop_tls_roundtrip(v in arb_tls()) { assert_roundtrip(&v); }

    #[test]
    fn prop_env_var_roundtrip(v in arb_env_var()) { assert_roundtrip(&v); }

    #[test]
    fn prop_backup_roundtrip(v in arb_backup()) { assert_roundtrip(&v); }

    #[test]
    fn prop_gpu_roundtrip(v in arb_gpu()) { assert_roundtrip(&v); }

    #[test]
    fn prop_network_policy_roundtrip(v in arb_network_policy()) { assert_roundtrip(&v); }

    #[test]
    fn prop_node_scheduling_roundtrip(v in arb_node_scheduling()) { assert_roundtrip(&v); }

    #[test]
    fn prop_probe_config_roundtrip(v in arb_probe_config()) { assert_roundtrip(&v); }

    #[test]
    fn prop_probe_spec_roundtrip(v in arb_probe_spec()) { assert_roundtrip(&v); }

    // #38: the core merge_with contract — empty user fields inherit the
    // default, non-empty user fields are never overwritten; digest/pull_policy
    // always pass through from the user value.
    #[test]
    fn prop_image_merge_with(user in arb_image(), default in arb_image()) {
        let merged = user.clone().merge_with(&default);
        let expected_repo =
            if user.repository.is_empty() { &default.repository } else { &user.repository };
        let expected_tag = if user.tag.is_empty() { &default.tag } else { &user.tag };
        prop_assert_eq!(&merged.repository, expected_repo);
        prop_assert_eq!(&merged.tag, expected_tag);
        prop_assert_eq!(&merged.digest, &user.digest);
        prop_assert_eq!(&merged.pull_policy, &user.pull_policy);
    }

    // camelCase stays stable for every generated ProbeConfig.
    #[test]
    fn prop_probe_config_camel_case(v in arb_probe_config()) {
        let obj = serde_json::to_value(&v).unwrap();
        let obj = obj.as_object().unwrap();
        prop_assert!(obj.contains_key("probeType"));
        prop_assert!(obj.contains_key("initialDelaySeconds"));
        prop_assert!(obj.contains_key("periodSeconds"));
        prop_assert!(obj.contains_key("timeoutSeconds"));
        prop_assert!(obj.contains_key("failureThreshold"));
        prop_assert!(!obj.contains_key("probe_type"));
    }

    // camelCase stays stable for every generated PvcVolume.
    #[test]
    fn prop_pvc_camel_case(v in arb_pvc()) {
        let obj = serde_json::to_value(&v).unwrap();
        let obj = obj.as_object().unwrap();
        prop_assert!(obj.contains_key("mountPath"));
        prop_assert!(obj.contains_key("accessMode"));
        prop_assert!(obj.contains_key("storageClass"));
        prop_assert!(obj.contains_key("existingClaimName"));
        prop_assert!(!obj.contains_key("mount_path"));
        prop_assert!(!obj.contains_key("access_mode"));
    }

    // camelCase stays stable for every generated SecurityProfile.
    #[test]
    fn prop_security_profile_camel_case(v in arb_security_profile()) {
        let obj = serde_json::to_value(&v).unwrap();
        let obj = obj.as_object().unwrap();
        prop_assert!(obj.contains_key("profileType"));
        prop_assert!(obj.contains_key("runAsNonRoot"));
        prop_assert!(obj.contains_key("readOnlyRootFilesystem"));
        prop_assert!(obj.contains_key("allowPrivilegeEscalation"));
        prop_assert!(obj.contains_key("capabilitiesAdd"));
        prop_assert!(obj.contains_key("capabilitiesDrop"));
        prop_assert!(!obj.contains_key("profile_type"));
    }

    // merge_with is idempotent: applying the same override twice == once.
    // Convention: receiver (override) wins, argument (base) is the fallback.
    #[test]
    fn prop_persistence_merge_idempotent(over in arb_persistence(), base in arb_persistence()) {
        let once = over.merge_with(&base);
        let twice = once.merge_with(&base);
        prop_assert_eq!(
            serde_json::to_value(&once).unwrap(),
            serde_json::to_value(&twice).unwrap()
        );
    }

    // merge_with never produces duplicate NFS mount names.
    #[test]
    fn prop_persistence_merge_nfs_dedup(over in arb_persistence(), base in arb_persistence()) {
        let merged = over.merge_with(&base);
        let mut seen = std::collections::HashSet::new();
        for m in &merged.nfs_mounts {
            prop_assert!(
                seen.insert(m.name.clone()),
                "duplicate NFS mount name after merge: {}",
                m.name
            );
        }
    }

    // merge_with replaces volumes wholesale when the override (receiver) is
    // non-empty, and falls back to the base (argument) otherwise.
    #[test]
    fn prop_persistence_merge_volumes_replace(
        over in arb_persistence(),
        base in arb_persistence(),
    ) {
        let merged = over.merge_with(&base);
        let expected = if over.volumes.is_empty() { &base.volumes } else { &over.volumes };
        prop_assert_eq!(
            serde_json::to_value(&merged.volumes).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
}

/// Every `AppType` variant survives a serde roundtrip.
#[test]
fn app_type_serde_roundtrip_all_variants() {
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
    for app in &all {
        let json = serde_json::to_string(app).expect("serialize");
        let back: AppType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*app, back, "AppType variant changed across roundtrip");
    }
    // as_str() values must be unique across variants.
    let labels: std::collections::HashSet<_> = all.iter().map(|a| a.as_str()).collect();
    assert_eq!(
        labels.len(),
        all.len(),
        "as_str() collision between AppType variants"
    );
}

#[test]
fn image_spec_defaults_inject() {
    let v: ImageSpec = serde_json::from_str("{}").expect("deserialize");
    assert_eq!(v.repository, "");
    assert_eq!(v.tag, "");
    assert_eq!(v.digest, "");
    assert_eq!(v.pull_policy, "IfNotPresent");
}

#[test]
fn pvc_defaults_inject() {
    let v: PvcVolume =
        serde_json::from_str(r#"{"name":"config","mountPath":"/config"}"#).expect("deserialize");
    assert_eq!(v.access_mode, "ReadWriteOnce");
    assert_eq!(v.size, "1Gi");
    assert_eq!(v.storage_class, "");
    assert_eq!(v.existing_claim_name, None);
}

#[test]
fn service_port_defaults_inject() {
    let v: ServicePort = serde_json::from_str(r#"{"name":"http","port":80}"#).expect("deserialize");
    assert_eq!(v.protocol, "TCP");
    assert_eq!(v.container_port, None);
    assert_eq!(v.host_port, None);
}

#[test]
fn backup_spec_defaults_inject() {
    let v: BackupSpec = serde_json::from_str("{}").expect("deserialize");
    assert!(!v.enabled);
    assert_eq!(v.schedule, "");
    assert_eq!(v.retention_count, 5);
}

#[test]
fn network_policy_defaults_inject() {
    let v: NetworkPolicyConfig = serde_json::from_str("{}").expect("deserialize");
    assert!(v.allow_same_namespace);
    assert!(v.allow_dns);
    assert!(!v.allow_internet_egress);
    assert!(v.denied_cidr_blocks.is_empty());
}

#[test]
fn probe_config_defaults_inject() {
    let v: ProbeConfig = serde_json::from_str("{}").expect("deserialize");
    assert!(matches!(v.probe_type, ProbeType::Http));
    // Field-level `#[serde(default)]` uses `String::default()` ("") here, NOT
    // the struct's `Default` impl ("/"). The numeric fields, by contrast, carry
    // explicit `default = "fn"` attributes, so they inject real values.
    assert_eq!(v.path, "");
    assert_eq!(v.initial_delay_seconds, 30);
    assert_eq!(v.period_seconds, 10);
    assert_eq!(v.timeout_seconds, 5);
    assert_eq!(v.failure_threshold, 5);
}

#[test]
fn probe_spec_merge_with_empty_path() {
    // Issue #59: Partial probe override should inherit path from defaults.
    // When a CR sets `probes: { liveness: { probeType: Http } }` (no path),
    // the path deserializes to "" (serde default), but should be merged
    // with the default path instead of replacing it entirely.
    let defaults = ProbeSpec {
        liveness: ProbeConfig {
            probe_type: ProbeType::Http,
            path: "/api/health".to_string(),
            ..Default::default()
        },
        readiness: Default::default(),
    };

    let user = ProbeSpec {
        liveness: ProbeConfig {
            probe_type: ProbeType::Http,
            path: "".to_string(), // Deserialized from CR without path field
            ..Default::default()
        },
        readiness: Default::default(),
    };

    let merged = user.merge_with(&defaults);

    // Empty path should inherit from defaults
    assert_eq!(merged.liveness.path, "/api/health");
    // Other fields should come from user spec
    assert!(matches!(merged.liveness.probe_type, ProbeType::Http));
}

#[test]
fn gateway_spec_deserialize_omitted_route_type() {
    // Issue #58: When a GatewaySpec is deserialized without a routeType field,
    // it should be distinguishable from an explicit routeType: Http.
    // Currently route_type is RouteType with serde default = Http,
    // which makes "omitted" and "explicitly Http" indistinguishable.
    // This test documents the current behavior; the fix requires making
    // route_type: Option<RouteType> so "unset" can be distinguished.

    // Deserialize without routeType field
    let json = r#"{ "hosts": ["example.com"] }"#;
    let spec: GatewaySpec = serde_json::from_str(json).expect("deserialize");

    // With the fix (route_type: Option<RouteType>), omitted routeType deserializes to None
    assert!(spec.route_type.is_none());
    // and merge_with will fall back to defaults
}

#[test]
fn gateway_spec_merge_with_tls_cert_issuer() {
    // Issue #60: GatewaySpec::merge_with should field-merge inner TlsSpec,
    // not just at the Option level. When a CR sets `tls: { enabled: true }`
    // (no cert_issuer), the empty cert_issuer should inherit from the
    // stack-level default instead of silently breaking cert creation.
    use servarr_crds::v1alpha1::TlsSpec;

    let defaults = GatewaySpec {
        enabled: None,
        route_type: None,
        parent_refs: vec![],
        hosts: vec!["example.com".to_string()],
        tls: Some(TlsSpec {
            enabled: true,
            cert_issuer: "letsencrypt".to_string(),
            secret_name: None,
        }),
    };

    // User spec with partial TLS: enabled but no cert_issuer
    let user = GatewaySpec {
        enabled: None,
        route_type: None,
        parent_refs: vec![],
        hosts: vec!["example.com".to_string()],
        tls: Some(TlsSpec {
            enabled: true,
            cert_issuer: "".to_string(), // Serde default, not explicitly set
            secret_name: None,
        }),
    };

    let merged = user.merge_with(&defaults);

    // TLS should be present
    assert!(merged.tls.is_some());
    let merged_tls = merged.tls.unwrap();
    // Empty cert_issuer should inherit from defaults
    assert_eq!(merged_tls.cert_issuer, "letsencrypt");
    // enabled should come from user
    assert!(merged_tls.enabled);
}

// MaintainerrSyncSpec serde coverage (#132): camelCase key stability and
// default injection for the new cross-app sync spec.
#[test]
fn maintainerr_sync_spec_roundtrip_and_camel_case() {
    let spec = MaintainerrSyncSpec {
        enabled: true,
        namespace_scope: Some("media".to_string()),
        plex_token_secret: None,
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    // namespace_scope must render as camelCase namespaceScope
    assert!(json.contains("\"namespaceScope\":\"media\""), "got: {json}");
    assert!(
        !json.contains("namespace_scope"),
        "snake_case leaked: {json}"
    );
    // roundtrip preserves the JSON representation
    let back: MaintainerrSyncSpec = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        serde_json::to_value(&spec).unwrap(),
        serde_json::to_value(&back).unwrap(),
    );
}

#[test]
fn maintainerr_sync_spec_defaults_inject() {
    // An empty object must deserialize to the documented defaults.
    let spec: MaintainerrSyncSpec = serde_json::from_str("{}").expect("deserialize empty");
    assert!(!spec.enabled, "enabled should default to false");
    assert_eq!(spec.namespace_scope, None);
    assert_eq!(spec.plex_token_secret, None);
}

// proptest: plex_token_secret Some(string) must round-trip as camelCase "plexTokenSecret"
proptest::proptest! {
    #[test]
    fn maintainerr_sync_spec_plex_token_secret_camel_case(s in "[a-z][a-z0-9-]{0,30}") {
        let spec = MaintainerrSyncSpec {
            enabled: false,
            namespace_scope: None,
            plex_token_secret: Some(s.clone()),
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        proptest::prop_assert!(
            json.contains(&format!("\"plexTokenSecret\":\"{s}\"")),
            "camelCase key missing or value wrong: {json}"
        );
        proptest::prop_assert!(!json.contains("plex_token_secret"), "snake_case leaked: {json}");
        let back: MaintainerrSyncSpec = serde_json::from_str(&json).expect("deserialize");
        proptest::prop_assert_eq!(back.plex_token_secret.as_deref(), Some(s.as_str()));
    }
}
