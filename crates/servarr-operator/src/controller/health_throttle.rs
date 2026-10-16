//! `apiHealthCheck.intervalSeconds` poll-window throttles (#506). Two gates: one for the
//! read-only health probe, one for the destructive self-heal pass.

use servarr_crds::Condition;
use tracing::debug;

use super::DEFAULT_HEALTH_CHECK_INTERVAL_SECONDS;

/// How [`is_health_poll_throttled`] treats an *existing* condition whose `lastTransitionTime`
/// fails to parse. A raw `bool` here is a misuse trap: a swapped literal at a call site
/// silently reintroduces #519's fail-open bug on a destructive caller with no compile error
/// and no test signal, so this is a dedicated type instead of a positional flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CorruptTimestampPolicy {
    /// Treat an unparseable timestamp as "not throttled" — run the poll. Safe for read-only
    /// callers (re-running a probe on corrupt data costs nothing extra).
    FailOpen,
    /// Treat an unparseable timestamp as throttled — skip, preserve/repair the existing
    /// condition, and log a warning. Required for callers gating a destructive operation.
    FailClosed,
}

/// `apiHealthCheck.intervalSeconds` throttle (#506): whether the health poll for
/// `condition_type` should be skipped because the existing condition's
/// `lastTransitionTime` still falls inside `interval_seconds` of now.
///
/// Only a positive health assertion (`status == "True"`) is rate-limited. An error,
/// `Unknown`, or `False` condition is never throttled, so recovery is re-probed on the
/// next reconcile instead of being frozen until the interval elapses. A missing timestamp,
/// a future-dated timestamp, or an unset/zero interval never throttles — the poll runs.
///
/// An *existing* condition whose `lastTransitionTime` fails to parse is ambiguous: it's cheap
/// to just re-run a read-only probe, but running an unthrottled destructive pass (torrent
/// removal) on corrupt status data every reconcile is not (#519). `policy` lets the caller
/// pick — see [`CorruptTimestampPolicy`].
pub(super) fn is_health_poll_throttled(
    existing: Option<&Condition>,
    interval_seconds: Option<u32>,
    now: &str,
    policy: CorruptTimestampPolicy,
) -> bool {
    let Some(existing) = existing else {
        return false;
    };
    if existing.status != "True" {
        return false;
    }
    is_within_poll_window(existing, interval_seconds, now, policy)
}

/// Destructive-path throttle (#844). Keys on `lastTransitionTime` for every status, not only
/// `True`. The self-heal pass writes its condition with the run's own time, so the timestamp
/// records the last destructive attempt, whatever status that attempt produced.
///
/// A corrupt timestamp fails closed (#519). The caller repairs the timestamp, so the pass is
/// not wedged on it.
pub(super) fn is_destructive_poll_throttled(
    existing: Option<&Condition>,
    interval_seconds: Option<u32>,
    now: &str,
) -> bool {
    existing.is_some_and(|condition| {
        is_within_poll_window(
            condition,
            interval_seconds,
            now,
            CorruptTimestampPolicy::FailClosed,
        )
    })
}

/// Whether `existing.lastTransitionTime` falls inside the poll window. Callers apply their
/// own status gate first.
fn is_within_poll_window(
    existing: &Condition,
    interval_seconds: Option<u32>,
    now: &str,
    policy: CorruptTimestampPolicy,
) -> bool {
    let interval = interval_seconds.unwrap_or(DEFAULT_HEALTH_CHECK_INTERVAL_SECONDS);
    let (Ok(last), Ok(current)) = (
        chrono::DateTime::parse_from_rfc3339(&existing.last_transition_time),
        chrono::DateTime::parse_from_rfc3339(now),
    ) else {
        // This helper cannot name the owning object. A fail-closed caller warns with the object.
        debug!(
            condition_type = %existing.condition_type,
            last_transition_time = %existing.last_transition_time,
            policy = ?policy,
            "poll window: unparseable lastTransitionTime"
        );
        return policy == CorruptTimestampPolicy::FailClosed;
    };
    // Fail open on a future-dated timestamp (clock skew, hand-edit): `current < last`
    // would otherwise throttle every poll — including at `intervalSeconds: 0` — until
    // the future passes.
    if current < last {
        return false;
    }
    current - last < chrono::Duration::seconds(i64::from(interval))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-10-16T12:00:00Z";

    fn condition(status: &str, last_transition_time: &str) -> Condition {
        Condition {
            condition_type: "DownloadDataHealthy".to_string(),
            status: status.to_string(),
            reason: "MissingDataDetected".to_string(),
            message: String::new(),
            last_transition_time: last_transition_time.to_string(),
        }
    }

    #[test]
    fn destructive_throttle_holds_false_status_inside_interval() {
        // #844: a torrent stuck in error 3 leaves the condition at False. The pass must still
        // wait out the interval instead of re-running torrent-verify on every reconcile.
        let existing = condition("False", "2026-10-16T11:59:50Z");
        assert!(is_destructive_poll_throttled(
            Some(&existing),
            Some(3600),
            NOW
        ));
    }

    #[test]
    fn destructive_throttle_holds_unknown_status_inside_interval() {
        let existing = condition("Unknown", "2026-10-16T11:59:50Z");
        assert!(is_destructive_poll_throttled(
            Some(&existing),
            Some(3600),
            NOW
        ));
    }

    #[test]
    fn destructive_throttle_releases_once_interval_elapsed() {
        let existing = condition("False", "2026-10-16T10:00:00Z");
        assert!(!is_destructive_poll_throttled(
            Some(&existing),
            Some(3600),
            NOW
        ));
    }

    #[test]
    fn destructive_throttle_never_applies_without_existing_condition() {
        assert!(!is_destructive_poll_throttled(None, Some(3600), NOW));
    }

    #[test]
    fn destructive_throttle_fails_closed_on_corrupt_timestamp() {
        let existing = condition("False", "not-a-timestamp");
        assert!(is_destructive_poll_throttled(
            Some(&existing),
            Some(3600),
            NOW
        ));
    }

    #[test]
    fn destructive_throttle_fails_open_on_future_timestamp() {
        let existing = condition("False", "2026-10-16T13:00:00Z");
        assert!(!is_destructive_poll_throttled(
            Some(&existing),
            Some(3600),
            NOW
        ));
    }

    #[test]
    fn destructive_throttle_uses_default_interval_when_omitted() {
        let inside = condition("False", "2026-10-16T11:59:30Z");
        let outside = condition("False", "2026-10-16T11:58:00Z");
        assert!(is_destructive_poll_throttled(Some(&inside), None, NOW));
        assert!(!is_destructive_poll_throttled(Some(&outside), None, NOW));
    }

    #[test]
    fn read_only_throttle_still_skips_non_true_status() {
        // The read-only probe keeps its status gate, so a failing app is re-probed each reconcile.
        let existing = condition("False", "2026-10-16T11:59:50Z");
        assert!(!is_health_poll_throttled(
            Some(&existing),
            Some(3600),
            NOW,
            CorruptTimestampPolicy::FailOpen
        ));
    }

    proptest::proptest! {
        // A corrupt timestamp must fail closed on the destructive path for any input (#519).
        #[test]
        fn destructive_throttle_fails_closed_on_any_unparseable_timestamp(raw in "\\PC*") {
            proptest::prop_assume!(chrono::DateTime::parse_from_rfc3339(&raw).is_err());
            let existing = condition("False", &raw);
            proptest::prop_assert!(is_destructive_poll_throttled(Some(&existing), Some(3600), NOW));
        }
    }
}
