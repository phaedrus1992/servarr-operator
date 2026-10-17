//! Event and condition text for a self-heal pass (#483, #538, #840).

use super::*;

pub(in crate::controller) async fn report_stale_torrents(
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    stale: &[servarr_api::TorrentInfo],
    outcome: &RemediationOutcome,
    held_back: usize,
) {
    // Nothing needed remediation by the time this pass finished (e.g. every stale torrent
    // recovered during the verify poll) — no Warning worth surfacing.
    if outcome.removed.is_empty()
        && outcome.confirmed_orphaned.is_empty()
        && outcome.still_pending.is_empty()
        && outcome.settled_with_data.is_empty()
        && outcome.rpc_failure.is_none()
        && held_back == 0
    {
        return;
    }

    // Torrent names come from tracker/`.torrent` metadata outside the operator's trust
    // boundary, so they must not reach a tenant-visible Event unsanitized (control
    // characters, unbounded length — `events.k8s.io/v1` rejects a note over 1024 chars).
    // Report the operator-assigned ids instead; names are available in the operator's own
    // debug log for troubleshooting (#483).
    //
    // For large batches the full id list overflows 1024 chars (486 torrents → 2448-char
    // note). Truncate to first 20 ids + "...and N more" to stay within the limit
    // regardless of batch size (#538).
    debug!(
        names = ?stale.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        "download-client health: stale torrent names"
    );
    let ids: Vec<i64> = stale.iter().map(|t| t.id).collect();
    let note = build_stale_torrent_note(&ids, stale.len(), outcome, held_back);
    publish_event(
        recorder,
        obj_ref,
        TenantEvent {
            type_: EventType::Warning,
            reason: "DownloadDataMissing",
            note: TenantSafeMessage::new(note),
            action: "DownloadClientHealthCheck",
        },
    )
    .await;
}

/// Build the event note for a stale-torrent batch, truncating the id list if necessary
/// to stay within the 1024-char limit on `events.k8s.io/v1` notes (#538).
pub(in crate::controller) fn build_stale_torrent_note(
    ids: &[i64],
    stale_count: usize,
    outcome: &RemediationOutcome,
    held_back: usize,
) -> String {
    const MAX_IDS_SHOWN: usize = 20;
    let truncated = ids.len().saturating_sub(MAX_IDS_SHOWN);
    let displayed: Vec<String> = ids
        .iter()
        .take(MAX_IDS_SHOWN)
        .map(|id| id.to_string())
        .collect();
    let ids_part = if displayed.is_empty() {
        "none".to_string()
    } else if truncated > 0 {
        format!("{} ...and {truncated} more", displayed.join(", "))
    } else {
        displayed.join(", ")
    };
    // With a failed RPC the hashes never reached verify, so "pending verify" would mislead (#845).
    let rpc_part = match outcome.rpc_failure {
        Some(op) => format!("; Transmission {op} failed, so no verify ran"),
        None => String::new(),
    };
    let held_part = if held_back > 0 {
        format!("; {held_back} held back, download directory may be unmounted")
    } else {
        String::new()
    };
    format!(
        "{stale_count} torrent(s) reporting missing data (ids: {ids_part}); \
         {} removed as orphaned, {} confirmed orphaned but not removed, \
         {} pending verify{held_part}{rpc_part}",
        outcome.removed.len(),
        outcome.confirmed_orphaned.len(),
        outcome.still_pending.len(),
    )
}

pub(in crate::controller) fn build_download_health_condition(
    stale_count: usize,
    held_back: usize,
    outcome: &RemediationOutcome,
    now: &str,
) -> Condition {
    // An RPC failure is not a data finding, so it wins over every data reason (#845).
    // Checked first, so a held-back directory cannot hide the failed call.
    if let Some(op) = outcome.rpc_failure {
        return Condition::unknown(
            condition_types::DOWNLOAD_DATA_HEALTHY,
            "RemediationRpcError",
            TenantSafeMessage::new(format!(
                "Transmission {op} failed during the self-heal pass; the pass retries after \
                 intervalSeconds"
            )),
            now,
        );
    }
    if held_back > 0 {
        return Condition::fail(
            condition_types::DOWNLOAD_DATA_HEALTHY,
            "DownloadDirUnavailable",
            TenantSafeMessage::new(format!(
                "{held_back} torrent(s) held back: every torrent in their download directory \
                 reports missing data, so the directory may be unmounted. Removal is skipped for \
                 them only ({} removed, {} confirmed orphaned but not removed, {} pending verify \
                 for the other torrents).",
                outcome.removed.len(),
                outcome.confirmed_orphaned.len(),
                outcome.still_pending.len(),
            )),
            now,
        );
    }
    let recovered = stale_count
        .saturating_sub(outcome.removed.len())
        .saturating_sub(outcome.confirmed_orphaned.len())
        .saturating_sub(outcome.still_pending.len())
        .saturating_sub(outcome.settled_with_data.len());
    if recovered == stale_count {
        return Condition::ok(
            condition_types::DOWNLOAD_DATA_HEALTHY,
            "DataRecovered",
            TenantSafeMessage::new(format!("{stale_count} torrent(s) recovered after verify")),
            now,
        );
    }
    Condition::fail(
        condition_types::DOWNLOAD_DATA_HEALTHY,
        "MissingDataDetected",
        TenantSafeMessage::new(format!(
            "{stale_count} torrent(s) reporting missing data ({recovered} recovered, {} removed, \
             {} confirmed orphaned but not removed, {} pending verify, {} settled with data \
             present)",
            outcome.removed.len(),
            outcome.confirmed_orphaned.len(),
            outcome.still_pending.len(),
            outcome.settled_with_data.len(),
        )),
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-10-16T12:00:00Z";

    #[test]
    fn rpc_failure_reports_remediation_error_not_pending_verify() {
        // #845: an RPC error must not read as "N pending verify" for every pass.
        let outcome = RemediationOutcome {
            still_pending: vec!["hash-1".to_string()],
            rpc_failure: Some("torrent-remove"),
            ..Default::default()
        };
        let cond = build_download_health_condition(1, 0, &outcome, NOW);
        assert_eq!(cond.status, "Unknown");
        assert_eq!(cond.reason, "RemediationRpcError");
        assert!(cond.message.contains("torrent-remove"), "{}", cond.message);
    }

    #[test]
    fn settled_with_data_is_counted_separately_from_pending() {
        // #845: a torrent that settled with error 3 and data present is its own outcome.
        let outcome = RemediationOutcome {
            settled_with_data: vec!["hash-1".to_string()],
            ..Default::default()
        };
        let cond = build_download_health_condition(1, 0, &outcome, NOW);
        assert_eq!(cond.reason, "MissingDataDetected");
        assert!(
            cond.message
                .contains("0 pending verify, 1 settled with data present")
        );
    }
    #[test]
    fn rpc_failure_wins_over_held_back_directory() {
        // A held-back directory must not hide a failed RPC in the same pass (#845).
        let outcome = RemediationOutcome {
            rpc_failure: Some("torrent-verify"),
            ..Default::default()
        };
        let cond = build_download_health_condition(3, 2, &outcome, NOW);
        assert_eq!(cond.reason, "RemediationRpcError");
    }

    #[test]
    fn event_note_says_no_verify_ran_after_rpc_failure() {
        let outcome = RemediationOutcome {
            still_pending: vec!["hash-1".to_string()],
            rpc_failure: Some("torrent-verify"),
            ..Default::default()
        };
        let note = build_stale_torrent_note(&[1], 1, &outcome, 0);
        assert!(
            note.contains("torrent-verify failed, so no verify ran"),
            "{note}"
        );
    }
}
