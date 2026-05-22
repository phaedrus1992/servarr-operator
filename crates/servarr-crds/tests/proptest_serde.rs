use proptest::prelude::*;
use servarr_crds::*;

// Bounded ISO 8601 timestamp components keep generated values semantically
// valid (month/day/hour/minute/second within real ranges).
fn arb_timestamp() -> impl Strategy<Value = String> {
    (
        1970u16..=2100,
        1u8..=12,
        1u8..=28,
        0u8..=23,
        0u8..=59,
        0u8..=59,
    )
        .prop_map(|(y, mo, d, h, mi, s)| {
            format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
        })
}

fn arb_condition() -> impl Strategy<Value = Condition> {
    (
        "[A-Za-z0-9_-]{1,50}",
        "[A-Za-z0-9_-]{1,50}",
        ".*",
        arb_timestamp(),
    )
        .prop_map(|(cond_type, reason, message, timestamp)| {
            Condition::ok(&cond_type, &reason, &message, &timestamp)
        })
}

fn arb_backup_status() -> impl Strategy<Value = Option<BackupStatus>> {
    prop_oneof![
        Just(None),
        (
            prop::option::of(".*"),
            prop::option::of(".*"),
            any::<u32>(),
        )
            .prop_map(|(last_backup_time, last_backup_result, backup_count)| Some(
                BackupStatus {
                    last_backup_time,
                    last_backup_result,
                    backup_count,
                }
            )),
    ]
}

fn arb_status() -> impl Strategy<Value = ServarrAppStatus> {
    (
        any::<bool>(),
        any::<i32>(),
        any::<i64>(),
        prop::collection::vec(arb_condition(), 0..10),
        arb_backup_status(),
    )
        .prop_map(
            |(ready, ready_replicas, observed_generation, conditions, backup_status)| {
                ServarrAppStatus {
                    ready,
                    ready_replicas,
                    observed_generation,
                    conditions,
                    backup_status,
                }
            },
        )
}

proptest! {
    #[test]
    fn prop_condition_serde_roundtrip(cond in arb_condition()) {
        let json = serde_json::to_string(&cond).expect("serialization failed");
        let deserialized: Condition = serde_json::from_str(&json).expect("deserialization failed");

        prop_assert_eq!(&deserialized.condition_type, &cond.condition_type);
        prop_assert_eq!(&deserialized.status, &cond.status);
        prop_assert_eq!(&deserialized.reason, &cond.reason);
        prop_assert_eq!(&deserialized.message, &cond.message);
        prop_assert_eq!(&deserialized.last_transition_time, &cond.last_transition_time);
    }

    #[test]
    fn prop_servarr_app_status_serde_roundtrip(status in arb_status()) {
        let json = serde_json::to_string(&status).expect("serialization failed");
        let deserialized: ServarrAppStatus = serde_json::from_str(&json).expect("deserialization failed");

        prop_assert_eq!(deserialized.ready, status.ready);
        prop_assert_eq!(deserialized.ready_replicas, status.ready_replicas);
        prop_assert_eq!(deserialized.observed_generation, status.observed_generation);
        prop_assert_eq!(deserialized.conditions.len(), status.conditions.len());

        for (orig, deser) in status.conditions.iter().zip(deserialized.conditions.iter()) {
            prop_assert_eq!(&deser.condition_type, &orig.condition_type);
            prop_assert_eq!(&deser.status, &orig.status);
            prop_assert_eq!(&deser.reason, &orig.reason);
            prop_assert_eq!(&deser.message, &orig.message);
            prop_assert_eq!(&deser.last_transition_time, &orig.last_transition_time);
        }

        match (&status.backup_status, &deserialized.backup_status) {
            (None, None) => {}
            (Some(orig), Some(deser)) => {
                prop_assert_eq!(&deser.last_backup_time, &orig.last_backup_time);
                prop_assert_eq!(&deser.last_backup_result, &orig.last_backup_result);
                prop_assert_eq!(deser.backup_count, orig.backup_count);
            }
            _ => prop_assert!(false, "backup_status Option variant changed across roundtrip"),
        }
    }

    // Verify camelCase rename unconditionally via parsed JSON structure rather
    // than substring matching on the serialized text.
    #[test]
    fn prop_json_uses_camel_case(status in arb_status()) {
        let json = serde_json::to_string(&status).expect("serialization failed");
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON parse failed");
        let obj = value.as_object().expect("status is not a JSON object");

        prop_assert!(obj.contains_key("readyReplicas"));
        prop_assert!(obj.contains_key("observedGeneration"));
        prop_assert!(!obj.contains_key("ready_replicas"));
        prop_assert!(!obj.contains_key("observed_generation"));

        for cond in obj.get("conditions").and_then(|c| c.as_array()).into_iter().flatten() {
            let cond_obj = cond.as_object().expect("condition is not an object");
            prop_assert!(cond_obj.contains_key("conditionType"));
            prop_assert!(cond_obj.contains_key("lastTransitionTime"));
            prop_assert!(!cond_obj.contains_key("condition_type"));
            prop_assert!(!cond_obj.contains_key("last_transition_time"));
        }
    }

    #[test]
    fn prop_condition_handles_unicode(unicode_str in r"(\PC|\p{L}|\p{N}|\s){0,100}") {
        let cond = Condition::ok("Test", "Reason", &unicode_str, "2025-01-01T00:00:00Z");
        let json = serde_json::to_string(&cond).expect("serialization failed");
        let deserialized: Condition = serde_json::from_str(&json).expect("deserialization failed");

        prop_assert_eq!(deserialized.message, unicode_str);
    }
}
