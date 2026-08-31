use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts};

lazy_static::lazy_static! {
    pub static ref RECONCILE_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_reconcile_total",
            "Total number of reconciliations"
        ),
        &["app_type", "result"]
    )
    .unwrap();

    pub static ref RECONCILE_DURATION: HistogramVec = prometheus::register_histogram_vec!(
        HistogramOpts::new(
            "servarr_operator_reconcile_duration_seconds",
            "Duration of reconciliations in seconds"
        ),
        &["app_type"]
    )
    .unwrap();

    pub static ref DRIFT_CORRECTIONS_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_drift_corrections_total",
            "Total number of drift corrections applied"
        ),
        &["app_type", "namespace", "resource_type"]
    )
    .unwrap();

    pub static ref BACKUP_OPERATIONS_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_backup_operations_total",
            "Total number of backup and restore operations"
        ),
        &["app_type", "operation", "result"]
    )
    .unwrap();

    pub static ref MANAGED_APPS: IntGaugeVec = prometheus::register_int_gauge_vec!(
        Opts::new(
            "servarr_operator_managed_apps",
            "Number of managed apps per type and namespace"
        ),
        &["app_type", "namespace"]
    )
    .unwrap();

    pub static ref STACK_RECONCILE_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_stack_reconcile_total",
            "Total number of MediaStack reconciliations"
        ),
        &["result"]
    )
    .unwrap();

    pub static ref STACK_RECONCILE_DURATION: HistogramVec = prometheus::register_histogram_vec!(
        HistogramOpts::new(
            "servarr_operator_stack_reconcile_duration_seconds",
            "Duration of MediaStack reconciliations in seconds"
        ),
        &[]
    )
    .unwrap();

    pub static ref MANAGED_STACKS: IntGaugeVec = prometheus::register_int_gauge_vec!(
        Opts::new(
            "servarr_operator_managed_stacks",
            "Number of managed MediaStacks per namespace"
        ),
        &["namespace"]
    )
    .unwrap();

    pub static ref MANAGED_STACKS_LIST_FAILURES_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_managed_stacks_list_failures_total",
            "Total number of failures listing MediaStacks to refresh the managed-stacks gauge"
        ),
        &[]
    )
    .unwrap();

    pub static ref EVENT_PUBLISH_FAILURES_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_event_publish_failures_total",
            "Total number of Kubernetes Event publish failures, by event reason"
        ),
        &["reason"]
    )
    .unwrap();

    pub static ref MANAGED_APPS_LIST_FAILURES_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_managed_apps_list_failures_total",
            "Total number of failures listing ServarrApps to refresh the managed-apps gauge"
        ),
        &[]
    )
    .unwrap();
}

pub(crate) fn increment_reconcile_total(app_type: &str, result: &str) {
    RECONCILE_TOTAL.with_label_values(&[app_type, result]).inc();
}

pub(crate) fn observe_reconcile_duration(app_type: &str, duration_secs: f64) {
    RECONCILE_DURATION
        .with_label_values(&[app_type])
        .observe(duration_secs);
}

pub(crate) fn increment_drift_corrections(app_type: &str, namespace: &str, resource_type: &str) {
    DRIFT_CORRECTIONS_TOTAL
        .with_label_values(&[app_type, namespace, resource_type])
        .inc();
}

pub(crate) fn increment_backup_operations(app_type: &str, operation: &str, result: &str) {
    BACKUP_OPERATIONS_TOTAL
        .with_label_values(&[app_type, operation, result])
        .inc();
}

pub(crate) fn set_managed_apps(app_type: &str, namespace: &str, count: i64) {
    MANAGED_APPS
        .with_label_values(&[app_type, namespace])
        .set(count);
}

pub(crate) fn increment_managed_apps_list_failure() {
    MANAGED_APPS_LIST_FAILURES_TOTAL
        .with_label_values(&[] as &[&str])
        .inc();
}

pub(crate) fn increment_stack_reconcile_total(result: &str) {
    STACK_RECONCILE_TOTAL.with_label_values(&[result]).inc();
}

pub(crate) fn observe_stack_reconcile_duration(duration_secs: f64) {
    STACK_RECONCILE_DURATION
        .with_label_values(&[] as &[&str])
        .observe(duration_secs);
}

pub(crate) fn set_managed_stacks(namespace: &str, count: i64) {
    MANAGED_STACKS.with_label_values(&[namespace]).set(count);
}

pub(crate) fn increment_managed_stacks_list_failure() {
    MANAGED_STACKS_LIST_FAILURES_TOTAL
        .with_label_values(&[] as &[&str])
        .inc();
}

pub(crate) fn increment_event_publish_failure(reason: &str) {
    EVENT_PUBLISH_FAILURES_TOTAL
        .with_label_values(&[reason])
        .inc();
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use the lazy_static metric references directly to read values.
    // This avoids the protobuf API entirely and is simpler.

    #[test]
    fn increment_reconcile_total_increments_counter() {
        let before = RECONCILE_TOTAL
            .with_label_values(&["test_reconcile", "success"])
            .get();
        increment_reconcile_total("test_reconcile", "success");
        let after = RECONCILE_TOTAL
            .with_label_values(&["test_reconcile", "success"])
            .get();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn observe_reconcile_duration_records_histogram() {
        let before = RECONCILE_DURATION
            .with_label_values(&["test_duration"])
            .get_sample_count();
        observe_reconcile_duration("test_duration", 1.5);
        let after = RECONCILE_DURATION
            .with_label_values(&["test_duration"])
            .get_sample_count();
        assert_eq!(after, before + 1);
        assert!(
            RECONCILE_DURATION
                .with_label_values(&["test_duration"])
                .get_sample_sum()
                >= 1.5
        );
    }

    #[test]
    fn increment_event_publish_failure_increments_counter() {
        let before = EVENT_PUBLISH_FAILURES_TOTAL
            .with_label_values(&["TestReason"])
            .get();
        increment_event_publish_failure("TestReason");
        let after = EVENT_PUBLISH_FAILURES_TOTAL
            .with_label_values(&["TestReason"])
            .get();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn increment_managed_apps_list_failure_increments_counter() {
        let before = MANAGED_APPS_LIST_FAILURES_TOTAL
            .with_label_values(&[] as &[&str])
            .get();
        increment_managed_apps_list_failure();
        let after = MANAGED_APPS_LIST_FAILURES_TOTAL
            .with_label_values(&[] as &[&str])
            .get();
        // `>` rather than `==`: controller.rs's update_managed_apps_gauge test hits this same
        // label combo on the same process-wide counter and may run concurrently.
        assert!(after > before, "before={before}, after={after}");
    }

    #[test]
    fn increment_managed_stacks_list_failure_increments_counter() {
        let before = MANAGED_STACKS_LIST_FAILURES_TOTAL
            .with_label_values(&[] as &[&str])
            .get();
        increment_managed_stacks_list_failure();
        let after = MANAGED_STACKS_LIST_FAILURES_TOTAL
            .with_label_values(&[] as &[&str])
            .get();
        // `>` rather than `==`: media_stack_controller.rs's update_managed_stacks_gauge test
        // hits this same label combo on the same process-wide counter and may run concurrently.
        assert!(after > before, "before={before}, after={after}");
    }

    #[test]
    fn increment_drift_corrections_increments_counter() {
        let before = DRIFT_CORRECTIONS_TOTAL
            .with_label_values(&["test_drift", "testns", "Deployment"])
            .get();
        increment_drift_corrections("test_drift", "testns", "Deployment");
        let after = DRIFT_CORRECTIONS_TOTAL
            .with_label_values(&["test_drift", "testns", "Deployment"])
            .get();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn increment_backup_operations_increments_counter() {
        let before = BACKUP_OPERATIONS_TOTAL
            .with_label_values(&["test_backup", "backup", "success"])
            .get();
        increment_backup_operations("test_backup", "backup", "success");
        let after = BACKUP_OPERATIONS_TOTAL
            .with_label_values(&["test_backup", "backup", "success"])
            .get();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn set_managed_apps_sets_gauge() {
        set_managed_apps("test_gauge_app", "test_ns", 3);
        let val = MANAGED_APPS
            .with_label_values(&["test_gauge_app", "test_ns"])
            .get();
        assert_eq!(val, 3);

        // Setting again overwrites
        set_managed_apps("test_gauge_app", "test_ns", 7);
        let val = MANAGED_APPS
            .with_label_values(&["test_gauge_app", "test_ns"])
            .get();
        assert_eq!(val, 7);
    }

    #[test]
    fn increment_stack_reconcile_total_increments_counter() {
        let before = STACK_RECONCILE_TOTAL
            .with_label_values(&["test_stack_result"])
            .get();
        increment_stack_reconcile_total("test_stack_result");
        let after = STACK_RECONCILE_TOTAL
            .with_label_values(&["test_stack_result"])
            .get();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn observe_stack_reconcile_duration_records_histogram() {
        let before = STACK_RECONCILE_DURATION
            .with_label_values(&[] as &[&str])
            .get_sample_count();
        observe_stack_reconcile_duration(1.5);
        let after = STACK_RECONCILE_DURATION
            .with_label_values(&[] as &[&str])
            .get_sample_count();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn set_managed_stacks_sets_gauge() {
        set_managed_stacks("test_stack_ns", 2);
        let val = MANAGED_STACKS.with_label_values(&["test_stack_ns"]).get();
        assert_eq!(val, 2);

        // Setting again overwrites
        set_managed_stacks("test_stack_ns", 5);
        let val = MANAGED_STACKS.with_label_values(&["test_stack_ns"]).get();
        assert_eq!(val, 5);
    }

    #[test]
    fn metrics_appear_in_prometheus_gather() {
        // Trigger at least one metric so the family is populated.
        increment_reconcile_total("gather_test", "success");
        let families = prometheus::gather();
        let names: Vec<&str> = families.iter().map(|f| f.name()).collect();
        assert!(names.contains(&"servarr_operator_reconcile_total"));
    }
}
