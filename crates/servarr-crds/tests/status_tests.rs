use servarr_crds::*;

// ---------------------------------------------------------------------------
// Condition::ok()
// ---------------------------------------------------------------------------

#[test]
fn condition_ok_creates_true_condition_with_all_fields() {
    let cond = Condition::ok(
        condition_types::READY,
        "AllReplicasReady",
        "1 replica(s) available",
        "2025-06-01T12:00:00Z",
    );

    assert_eq!(cond.condition_type, "Ready");
    assert_eq!(cond.status, "True");
    assert_eq!(cond.reason, "AllReplicasReady");
    assert_eq!(cond.message, "1 replica(s) available");
    assert_eq!(cond.last_transition_time, "2025-06-01T12:00:00Z");
}

// ---------------------------------------------------------------------------
// Condition::fail()
// ---------------------------------------------------------------------------

#[test]
fn condition_fail_creates_false_condition_with_all_fields() {
    let cond = Condition::fail(
        condition_types::DEGRADED,
        "PodCrashLooping",
        "container restarted 5 times",
        "2025-06-01T13:00:00Z",
    );

    assert_eq!(cond.condition_type, "Degraded");
    assert_eq!(cond.status, "False");
    assert_eq!(cond.reason, "PodCrashLooping");
    assert_eq!(cond.message, "container restarted 5 times");
    assert_eq!(cond.last_transition_time, "2025-06-01T13:00:00Z");
}

// ---------------------------------------------------------------------------
// ServarrAppStatus::set_condition() -- insert new
// ---------------------------------------------------------------------------

#[test]
fn set_condition_inserts_new_condition_into_empty_status() {
    let mut status = ServarrAppStatus::default();
    assert!(status.conditions.is_empty());

    let cond = Condition::ok(
        condition_types::READY,
        "Ready",
        "all good",
        "2025-06-01T00:00:00Z",
    );
    status.set_condition(cond);

    assert_eq!(status.conditions.len(), 1);
    assert_eq!(status.conditions[0].condition_type, "Ready");
    assert_eq!(status.conditions[0].status, "True");
}

// ---------------------------------------------------------------------------
// ServarrAppStatus::set_condition() -- update existing (same type)
// ---------------------------------------------------------------------------

#[test]
fn set_condition_updates_existing_condition_of_same_type() {
    let mut status = ServarrAppStatus::default();

    // Insert initial condition
    status.set_condition(Condition::ok(
        condition_types::DEPLOYMENT_READY,
        "Deployed",
        "deployment rolled out",
        "2025-06-01T00:00:00Z",
    ));
    assert_eq!(status.conditions.len(), 1);
    assert_eq!(status.conditions[0].status, "True");

    // Update the same type to fail
    status.set_condition(Condition::fail(
        condition_types::DEPLOYMENT_READY,
        "RolloutFailed",
        "replica set timed out",
        "2025-06-01T01:00:00Z",
    ));

    // Length unchanged; values updated
    assert_eq!(status.conditions.len(), 1);
    assert_eq!(status.conditions[0].condition_type, "DeploymentReady");
    assert_eq!(status.conditions[0].status, "False");
    assert_eq!(status.conditions[0].reason, "RolloutFailed");
    assert_eq!(status.conditions[0].message, "replica set timed out");
    assert_eq!(
        status.conditions[0].last_transition_time,
        "2025-06-01T01:00:00Z"
    );
}

// ---------------------------------------------------------------------------
// ServarrAppStatus::set_condition() -- multiple different conditions
// ---------------------------------------------------------------------------

#[test]
fn set_condition_tracks_multiple_different_condition_types() {
    let mut status = ServarrAppStatus::default();

    status.set_condition(Condition::ok(
        condition_types::READY,
        "Ready",
        "ok",
        "2025-06-01T00:00:00Z",
    ));
    status.set_condition(Condition::ok(
        condition_types::SERVICE_READY,
        "ServiceCreated",
        "ClusterIP assigned",
        "2025-06-01T00:00:01Z",
    ));
    status.set_condition(Condition::fail(
        condition_types::NETWORK_POLICY_READY,
        "PolicyMissing",
        "not yet created",
        "2025-06-01T00:00:02Z",
    ));

    assert_eq!(status.conditions.len(), 3);

    // Verify each condition by type
    let ready = status
        .conditions
        .iter()
        .find(|c| c.condition_type == "Ready")
        .expect("Ready condition missing");
    assert_eq!(ready.status, "True");

    let svc = status
        .conditions
        .iter()
        .find(|c| c.condition_type == "ServiceReady")
        .expect("ServiceReady condition missing");
    assert_eq!(svc.status, "True");
    assert_eq!(svc.reason, "ServiceCreated");

    let np = status
        .conditions
        .iter()
        .find(|c| c.condition_type == "NetworkPolicyReady")
        .expect("NetworkPolicyReady condition missing");
    assert_eq!(np.status, "False");

    // Now update one of them and verify length stays at 3
    status.set_condition(Condition::ok(
        condition_types::NETWORK_POLICY_READY,
        "PolicyReady",
        "network policy applied",
        "2025-06-01T00:01:00Z",
    ));
    assert_eq!(status.conditions.len(), 3);
    let np_updated = status
        .conditions
        .iter()
        .find(|c| c.condition_type == "NetworkPolicyReady")
        .unwrap();
    assert_eq!(np_updated.status, "True");
    assert_eq!(np_updated.reason, "PolicyReady");
}

// ---------------------------------------------------------------------------
// BackupStatus default values
// ---------------------------------------------------------------------------

#[test]
fn backup_status_defaults() {
    let bs = BackupStatus::default();
    assert!(bs.last_backup_time.is_none());
    assert!(bs.last_backup_result.is_none());
    assert_eq!(bs.backup_count, 0);
}

// ---------------------------------------------------------------------------
// Serialization roundtrip of ServarrAppStatus with conditions
// ---------------------------------------------------------------------------

#[test]
fn servarr_app_status_serde_roundtrip_with_conditions_and_backup() {
    let status = ServarrAppStatus {
        ready: true,
        ready_replicas: 2,
        observed_generation: 42,
        conditions: vec![
            Condition::ok(
                condition_types::READY,
                "AllGood",
                "all replicas available",
                "2025-06-01T10:00:00Z",
            ),
            Condition::fail(
                condition_types::UPDATE_AVAILABLE,
                "NewVersion",
                "v4.1.0 available",
                "2025-06-01T11:00:00Z",
            ),
        ],
        backup_status: Some(BackupStatus {
            last_backup_time: Some("2025-06-01T03:00:00Z".into()),
            last_backup_result: Some("Success".into()),
            backup_count: 7,
        }),
    };

    let json = serde_json::to_string(&status).unwrap();
    let deserialized: ServarrAppStatus = serde_json::from_str(&json).unwrap();

    assert!(deserialized.ready);
    assert_eq!(deserialized.ready_replicas, 2);
    assert_eq!(deserialized.observed_generation, 42);
    assert_eq!(deserialized.conditions.len(), 2);

    assert_eq!(deserialized.conditions[0].condition_type, "Ready");
    assert_eq!(deserialized.conditions[0].status, "True");
    assert_eq!(deserialized.conditions[1].condition_type, "UpdateAvailable");
    assert_eq!(deserialized.conditions[1].status, "False");

    let backup = deserialized.backup_status.expect("backup_status missing");
    assert_eq!(
        backup.last_backup_time.as_deref(),
        Some("2025-06-01T03:00:00Z")
    );
    assert_eq!(backup.last_backup_result.as_deref(), Some("Success"));
    assert_eq!(backup.backup_count, 7);
}

#[test]
fn servarr_app_status_serde_roundtrip_camel_case_keys() {
    let status = ServarrAppStatus {
        ready: false,
        ready_replicas: 0,
        observed_generation: 1,
        conditions: vec![Condition::ok(
            condition_types::PROGRESSING,
            "NewReplicaSet",
            "creating pod",
            "2025-06-01T00:00:00Z",
        )],
        backup_status: None,
    };

    let json = serde_json::to_string(&status).unwrap();
    // Verify camelCase serialization
    assert!(json.contains("readyReplicas"));
    assert!(json.contains("observedGeneration"));
    assert!(json.contains("conditionType"));
    assert!(json.contains("lastTransitionTime"));
    assert!(json.contains("backupStatus"));
}

// ---------------------------------------------------------------------------
// condition_types constants
// ---------------------------------------------------------------------------

#[test]
fn condition_types_constants_are_correct() {
    assert_eq!(condition_types::READY, "Ready");
    assert_eq!(condition_types::DEPLOYMENT_READY, "DeploymentReady");
    assert_eq!(condition_types::SERVICE_READY, "ServiceReady");
    assert_eq!(condition_types::NETWORK_POLICY_READY, "NetworkPolicyReady");
    assert_eq!(condition_types::ROUTE_READY, "RouteReady");
    assert_eq!(condition_types::PVC_READY, "PvcReady");
    assert_eq!(condition_types::PROGRESSING, "Progressing");
    assert_eq!(condition_types::DEGRADED, "Degraded");
    assert_eq!(condition_types::APP_HEALTHY, "AppHealthy");
    assert_eq!(condition_types::UPDATE_AVAILABLE, "UpdateAvailable");
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn condition_ok_and_fail_with_empty_strings() {
    let ok = Condition::ok("", "", "", "");
    assert_eq!(ok.status, "True");
    assert!(ok.condition_type.is_empty());

    let fail = Condition::fail("", "", "", "");
    assert_eq!(fail.status, "False");
    assert!(fail.condition_type.is_empty());
}

#[test]
fn set_condition_on_status_with_preexisting_conditions() {
    let mut status = ServarrAppStatus {
        conditions: vec![
            Condition::ok("Alpha", "R1", "m1", "t1"),
            Condition::ok("Beta", "R2", "m2", "t2"),
            Condition::ok("Gamma", "R3", "m3", "t3"),
        ],
        ..Default::default()
    };

    // Update the middle one
    status.set_condition(Condition::fail("Beta", "Failed", "oops", "t4"));
    assert_eq!(status.conditions.len(), 3);
    assert_eq!(status.conditions[1].status, "False");
    assert_eq!(status.conditions[1].reason, "Failed");

    // Append a new one
    status.set_condition(Condition::ok("Delta", "R4", "m4", "t5"));
    assert_eq!(status.conditions.len(), 4);
    assert_eq!(status.conditions[3].condition_type, "Delta");
}

#[test]
fn default_servarr_app_status_is_not_ready() {
    let status = ServarrAppStatus::default();
    assert!(!status.ready);
    assert_eq!(status.ready_replicas, 0);
    assert_eq!(status.observed_generation, 0);
    assert!(status.conditions.is_empty());
    assert!(status.backup_status.is_none());
}
