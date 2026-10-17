//! Verify-then-remove pass for torrents that reported missing data (#483, #500, #840).

use super::*;

/// Hashes of settled torrents, split by what the settle shows. A torrent with error 3 lands in
/// exactly one set. A torrent without that error lands in none (#840, #845, #856).
#[derive(Debug, Default, PartialEq)]
struct SettledBuckets {
    /// Still at 0% after verify: the data is gone.
    confirmed_missing: Vec<String>,
    /// Error 3 with some data on disk, such as a permission or disk-full error.
    with_data: Vec<String>,
    /// Error 3 and no `percentDone`, so the data state is not known.
    unknown: Vec<String>,
}

fn classify_settled(settled: &[servarr_api::TorrentInfo]) -> SettledBuckets {
    let mut buckets = SettledBuckets::default();
    for t in settled.iter().filter(|t| is_missing_data_torrent(t)) {
        let hash = t.hash_string.clone();
        if is_confirmed_missing(t) {
            buckets.confirmed_missing.push(hash);
        } else if t.percent_done.is_none() {
            buckets.unknown.push(hash);
        } else {
            buckets.with_data.push(hash);
        }
    }
    buckets
}

/// Trigger `torrent-verify` on every stale torrent, wait for the verify to settle, then remove
/// any that are still reporting missing data -- Transmission's own re-check is the safety gate
/// that distinguishes "transient" from "genuinely gone" (#483). Torrents are addressed by their
/// stable `hashString`, not the process-local numeric `id`, which can be reassigned to a
/// different torrent if Transmission restarts inside the detect-to-remediate window (#500).
/// Removal is skipped -- confirmed-orphaned torrents are reported but left alone -- unless
/// `auto_remove` is set (#498).
pub(in crate::controller) async fn remediate_stale_torrents(
    tx_client: &servarr_api::TransmissionClient,
    stale: &[servarr_api::TorrentInfo],
    auto_remove: bool,
) -> RemediationOutcome {
    // Torrents Transmission is already hash-checking (e.g. verify was triggered on a
    // previous reconcile and hasn't finished) must not be re-verified — restarting the
    // hash-check on every 300s reconcile means a large torrent's verify never converges
    // and disk I/O never lets up (#483).
    let already_checking: Vec<String> = stale
        .iter()
        .filter(|t| is_checking(t))
        .map(|t| t.hash_string.clone())
        .collect();
    let hashes: Vec<String> = stale
        .iter()
        .filter(|t| !is_checking(t))
        .map(|t| t.hash_string.clone())
        .collect();
    if hashes.is_empty() {
        return RemediationOutcome {
            still_pending: already_checking,
            ..Default::default()
        };
    }
    let hash_refs: Vec<&str> = hashes.iter().map(String::as_str).collect();

    if let Err(e) = tx_client.torrent_verify(&hash_refs).await {
        warn!(error = %e.log_summary(), "download-client health: torrent-verify failed");
        return rpc_failed_outcome("torrent-verify", hashes, already_checking);
    }

    let settled = match poll_until_settled(tx_client, &hash_refs).await {
        Ok(settled) => settled,
        Err(op) => return rpc_failed_outcome(op, hashes, already_checking),
    };
    // Only a torrent still at 0% after verify is removed (#840). One that still errors with
    // data present is left for the next reconcile, like a torrent whose verify has not settled.
    let SettledBuckets {
        confirmed_missing: still_erroring,
        with_data: settled_with_data,
        unknown: settled_unknown,
    } = classify_settled(&settled);
    let mut still_checking: Vec<String> = hashes
        .into_iter()
        .filter(|h| !settled.iter().any(|t| &t.hash_string == h))
        .collect();
    still_checking.extend(already_checking);

    let base = RemediationOutcome {
        still_pending: still_checking,
        settled_with_data,
        settled_unknown,
        ..Default::default()
    };
    // An empty still_erroring is safe to pass on: torrent_remove(&[]) is a no-op.
    if !auto_remove {
        return RemediationOutcome {
            confirmed_orphaned: still_erroring,
            ..base
        };
    }
    remove_confirmed(tx_client, still_erroring, base).await
}

/// Outcome for a pass that stopped on an RPC error. The torrents stay stale, so they are
/// reported as pending. `op` names the failed RPC, so the condition does not blame the data (#845).
fn rpc_failed_outcome(
    op: &'static str,
    hashes: Vec<String>,
    already_checking: Vec<String>,
) -> RemediationOutcome {
    let mut still_pending = hashes;
    still_pending.extend(already_checking);
    RemediationOutcome {
        still_pending,
        rpc_failure: Some(op),
        ..Default::default()
    }
}

/// Remove the confirmed-missing torrents and merge the result into `base`. A failed removal
/// leaves them confirmed-orphaned, not pending, so the condition shows the real state (#845).
async fn remove_confirmed(
    tx_client: &servarr_api::TransmissionClient,
    still_erroring: Vec<String>,
    base: RemediationOutcome,
) -> RemediationOutcome {
    let still_erroring_refs: Vec<&str> = still_erroring.iter().map(String::as_str).collect();
    match tx_client
        .torrent_remove(&still_erroring_refs, servarr_api::DeleteLocalData::No)
        .await
    {
        Ok(()) => RemediationOutcome {
            removed: still_erroring,
            ..base
        },
        Err(e) => {
            warn!(error = %e.log_summary(), "download-client health: torrent-remove failed");
            RemediationOutcome {
                confirmed_orphaned: still_erroring,
                rpc_failure: Some("torrent-remove"),
                ..base
            }
        }
    }
}

/// Poll `torrent-get` for `hashes` until none are still hash-checking, or
/// [`VERIFY_POLL_ATTEMPTS`] is exhausted. Returns `Ok(empty)` on timeout, which callers read as
/// "still pending". Returns `Err` with the RPC name on an RPC error, so the caller reports the
/// failure, not a pending verify (#845). Neither case is grounds for removal.
pub(in crate::controller) async fn poll_until_settled(
    tx_client: &servarr_api::TransmissionClient,
    hashes: &[&str],
) -> Result<Vec<servarr_api::TorrentInfo>, &'static str> {
    poll_with_budget(
        tx_client,
        hashes,
        VERIFY_POLL_ATTEMPTS,
        VERIFY_POLL_INTERVAL,
    )
    .await
}

/// [`poll_until_settled`] with an explicit attempt budget and interval. The tests use a short
/// budget: a paused-clock test would let the HTTP request timeout fire during idle auto-advance.
pub(in crate::controller) async fn poll_with_budget(
    tx_client: &servarr_api::TransmissionClient,
    hashes: &[&str],
    attempts: u8,
    interval: Duration,
) -> Result<Vec<servarr_api::TorrentInfo>, &'static str> {
    // Transmission applies torrent-verify's status transition asynchronously relative to the
    // RPC response returning, so polling immediately risks reading the pre-verify snapshot —
    // still showing the old error with status 0 (not checking) — and wrongly treating the
    // torrent as "settled and still broken" before Transmission ever re-checked it (#483).
    // Sleep first to give the transition time to land.
    tokio::time::sleep(interval).await;
    for _ in 0..attempts {
        match tx_client
            .torrent_get(&TORRENT_HEALTH_FIELDS, Some(hashes))
            .await
        {
            Ok(t) if t.iter().all(|x| !is_checking(x)) => return Ok(t),
            Ok(_) => tokio::time::sleep(interval).await,
            Err(e) => {
                warn!(error = %e.log_summary(), "download-client health: torrent-get (poll) failed");
                return Err("torrent-get");
            }
        }
    }
    // Poll window used up. Callers read the empty result as "still pending", so log it here to
    // keep a slow verify visible (#840 review).
    warn!(
        hashes = hashes.len(),
        "download-client health: verify did not settle in the poll window"
    );
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn torrent(id: usize, error: i64, percent_done: Option<f64>) -> servarr_api::TorrentInfo {
        servarr_api::TorrentInfo {
            id: i64::try_from(id).unwrap_or(i64::MAX),
            name: String::new(),
            error: Some(error),
            error_string: String::new(),
            status: Some(0),
            hash_string: format!("hash-{id}"),
            percent_done,
            download_dir: "/downloads".to_string(),
        }
    }

    proptest! {
        /// Each error-3 torrent lands in exactly one set, and no other torrent lands in any.
        #[test]
        fn every_error_3_torrent_lands_in_exactly_one_bucket(
            specs in proptest::collection::vec(
                (prop_oneof![Just(0_i64), Just(2), Just(3)],
                 proptest::option::of(prop_oneof![Just(0.0_f64), Just(0.5), Just(1.0)])),
                0..24,
            ),
        ) {
            let torrents: Vec<_> = specs
                .iter()
                .enumerate()
                .map(|(i, (error, done))| torrent(i, *error, *done))
                .collect();
            let buckets = classify_settled(&torrents);
            for (i, (error, _)) in specs.iter().enumerate() {
                let hash = format!("hash-{i}");
                let hits = [&buckets.confirmed_missing, &buckets.with_data, &buckets.unknown]
                    .iter()
                    .filter(|set| set.contains(&hash))
                    .count();
                prop_assert_eq!(hits, usize::from(*error == TR_STAT_LOCAL_ERROR), "{}", hash);
            }
        }
    }

    #[test]
    fn unknown_progress_is_not_confirmed_missing_or_data_present() {
        let buckets = classify_settled(&[torrent(1, 3, None)]);
        assert_eq!(
            buckets,
            SettledBuckets {
                unknown: vec!["hash-1".to_string()],
                ..Default::default()
            }
        );
    }
}
