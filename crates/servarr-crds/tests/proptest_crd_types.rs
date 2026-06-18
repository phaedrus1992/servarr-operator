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
    prop_oneof![Just(None), any::<bool>().prop_map(Some)]
}

fn arb_opt_i32() -> impl Strategy<Value = Option<i32>> {
    prop_oneof![Just(None), any::<i32>().prop_map(Some)]
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

fn arb_image() -> impl Strategy<Value = ImageSpec> {
    (arb_string(), arb_string(), arb_string(), arb_string()).prop_map(
        |(repository, tag, digest, pull_policy)| ImageSpec {
            repository,
            tag,
            digest,
            pull_policy,
        },
    )
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
        arb_route_type(),
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
    #[test]
    fn prop_persistence_merge_idempotent(base in arb_persistence(), over in arb_persistence()) {
        let once = base.merge_with(&over);
        let twice = once.merge_with(&over);
        prop_assert_eq!(
            serde_json::to_value(&once).unwrap(),
            serde_json::to_value(&twice).unwrap()
        );
    }

    // merge_with never produces duplicate NFS mount names.
    #[test]
    fn prop_persistence_merge_nfs_dedup(base in arb_persistence(), over in arb_persistence()) {
        let merged = base.merge_with(&over);
        let mut seen = std::collections::HashSet::new();
        for m in &merged.nfs_mounts {
            prop_assert!(
                seen.insert(m.name.clone()),
                "duplicate NFS mount name after merge: {}",
                m.name
            );
        }
    }

    // merge_with replaces volumes wholesale when the override is non-empty,
    // and falls back to the base otherwise.
    #[test]
    fn prop_persistence_merge_volumes_replace(
        base in arb_persistence(),
        over in arb_persistence(),
    ) {
        let merged = base.merge_with(&over);
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
    assert_eq!(v.timeout_seconds, 1);
    assert_eq!(v.failure_threshold, 3);
}
