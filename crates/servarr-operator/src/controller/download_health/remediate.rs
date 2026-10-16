//! Verify-then-remove pass for torrents that reported missing data (#483, #500, #840).

use super::*;

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
        let mut still_pending = hashes;
        still_pending.extend(already_checking);
        return RemediationOutcome {
            still_pending,
            ..Default::default()
        };
    }

    let settled = poll_until_settled(tx_client, &hash_refs).await;
    // Only a torrent still at 0% after verify is removed (#840). One that still errors with
    // data present is left for the next reconcile, like a torrent whose verify has not settled.
    let still_erroring: Vec<String> = settled
        .iter()
        .filter(|t| is_confirmed_missing(t))
        .map(|t| t.hash_string.clone())
        .collect();
    let mut still_checking: Vec<String> = hashes
        .into_iter()
        .filter(|h| !settled.iter().any(|t| &t.hash_string == h))
        .collect();
    still_checking.extend(
        settled
            .iter()
            .filter(|t| is_missing_data_torrent(t) && !is_confirmed_missing(t))
            .map(|t| t.hash_string.clone()),
    );
    still_checking.extend(already_checking);

    // still_erroring.is_empty() falls through to the branches below rather than an early
    // return: with auto_remove=false that's already `confirmed_orphaned: [] , still_pending:
    // still_checking`, and with auto_remove=true, torrent_remove(&[]) is a documented no-op
    // that produces the identical outcome -- both paths already agree, no need to special-case.
    if !auto_remove {
        return RemediationOutcome {
            confirmed_orphaned: still_erroring,
            still_pending: still_checking,
            ..Default::default()
        };
    }

    let still_erroring_refs: Vec<&str> = still_erroring.iter().map(String::as_str).collect();
    match tx_client
        .torrent_remove(&still_erroring_refs, servarr_api::DeleteLocalData::No)
        .await
    {
        Ok(()) => RemediationOutcome {
            removed: still_erroring,
            still_pending: still_checking,
            ..Default::default()
        },
        Err(e) => {
            warn!(error = %e.log_summary(), "download-client health: torrent-remove failed");
            let mut still_pending = still_erroring;
            still_pending.extend(still_checking);
            RemediationOutcome {
                still_pending,
                ..Default::default()
            }
        }
    }
}

/// Poll `torrent-get` for `hashes` until none are still hash-checking, or
/// [`VERIFY_POLL_ATTEMPTS`] is exhausted. Returns an empty `Vec` on timeout or RPC error —
/// callers treat that as "still pending", never as grounds for removal.
pub(in crate::controller) async fn poll_until_settled(
    tx_client: &servarr_api::TransmissionClient,
    hashes: &[&str],
) -> Vec<servarr_api::TorrentInfo> {
    // Transmission applies torrent-verify's status transition asynchronously relative to the
    // RPC response returning, so polling immediately risks reading the pre-verify snapshot —
    // still showing the old error with status 0 (not checking) — and wrongly treating the
    // torrent as "settled and still broken" before Transmission ever re-checked it (#483).
    // Sleep first to give the transition time to land.
    tokio::time::sleep(VERIFY_POLL_INTERVAL).await;
    for _ in 0..VERIFY_POLL_ATTEMPTS {
        match tx_client
            .torrent_get(&TORRENT_HEALTH_FIELDS, Some(hashes))
            .await
        {
            Ok(t) if t.iter().all(|x| !is_checking(x)) => return t,
            Ok(_) => tokio::time::sleep(VERIFY_POLL_INTERVAL).await,
            Err(e) => {
                warn!(error = %e.log_summary(), "download-client health: torrent-get (poll) failed");
                return Vec::new();
            }
        }
    }
    Vec::new()
}
