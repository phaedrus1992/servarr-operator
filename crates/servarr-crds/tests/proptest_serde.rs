use proptest::prelude::*;
use servarr_crds::*;

// Property-based strategy for generating arbitrary Condition values
fn arb_condition() -> impl Strategy<Value = Condition> {
    (
        "[A-Za-z0-9_-]{1,50}",
        "[A-Za-z0-9_-]{1,50}",
        ".*",
        "[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z",
    )
        .prop_map(|(cond_type, reason, message, timestamp)| {
            Condition::ok(&cond_type, &reason, &message, &timestamp)
        })
}

// Property: Condition can be serialized and deserialized losslessly
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
}

// Property-based strategy for generating arbitrary ServarrAppStatus values
fn arb_status() -> impl Strategy<Value = ServarrAppStatus> {
    (
        any::<bool>(),
        0i32..100,
        0i64..1000,
        prop::collection::vec(arb_condition(), 0..10),
    )
        .prop_map(|(ready, ready_replicas, observed_generation, conditions)| {
            ServarrAppStatus {
                ready,
                ready_replicas,
                observed_generation,
                conditions,
                backup_status: None,
            }
        })
}

// Property: ServarrAppStatus can be serialized and deserialized losslessly
proptest! {
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
    }
}

// Property: Serialized JSON always uses camelCase
proptest! {
    #[test]
    fn prop_json_uses_camel_case(status in arb_status()) {
        let json = serde_json::to_string(&status).expect("serialization failed");

        // Check for camelCase keys
        if status.ready_replicas > 0 {
            prop_assert!(json.contains("readyReplicas"), "missing readyReplicas in camelCase");
        }
        if status.observed_generation > 0 {
            prop_assert!(json.contains("observedGeneration"), "missing observedGeneration in camelCase");
        }
        if !status.conditions.is_empty() {
            prop_assert!(json.contains("conditionType"), "missing conditionType in camelCase");
            prop_assert!(json.contains("lastTransitionTime"), "missing lastTransitionTime in camelCase");
        }
    }
}

// Property: Unicode strings are handled correctly
proptest! {
    #[test]
    fn prop_condition_handles_unicode(unicode_str in r"(\PC|\p{L}|\p{N}|\s){0,100}") {
        let cond = Condition::ok("Test", "Reason", &unicode_str, "2025-01-01T00:00:00Z");
        let json = serde_json::to_string(&cond).expect("serialization failed");
        let deserialized: Condition = serde_json::from_str(&json).expect("deserialization failed");

        prop_assert_eq!(deserialized.message, unicode_str);
    }
}
