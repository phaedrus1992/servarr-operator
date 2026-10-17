//! Transmission download-data self-heal (#483). Detects torrents whose data went missing,
//! re-verifies them, and removes the ones still broken once the verify settles.

use super::*;

pub(super) mod remediate;
pub(super) mod report;

/// How many times to re-poll a torrent's status after triggering `torrent-verify` before
/// treating it as "still checking, try again next reconcile" rather than removing it.
/// For large batches (hundreds of torrents) the verify itself can take minutes, so the
/// poll window must cover that (#537). 100 attempts × 500ms = 50s total, enough for
/// batches of several hundred torrents to complete verification.
pub(super) const VERIFY_POLL_ATTEMPTS: u8 = 100;
pub(super) const VERIFY_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Transmission RPC status codes meaning "still hash-checking" (queued-to-verify, verifying).
pub(super) const CHECKING_STATUSES: [i64; 2] = [1, 2];

/// `torrent-get` fields needed for self-heal detection and polling. A single shared list (not
/// duplicated per call site) so `hashString` can never silently drop from one of the two calls —
/// a `TorrentInfo` with a missing hash defaults to `""` (#500), which would scope subsequent
/// `torrent-verify`/`torrent-remove`/`torrent-get` calls to an empty-string id instead of failing
/// loudly.
pub(super) const TORRENT_HEALTH_FIELDS: [&str; 8] = [
    "id",
    "name",
    "error",
    "errorString",
    "status",
    "hashString",
    "percentDone",
    "downloadDir",
];

/// Transmission's `tr_stat` error code for "the torrent has a local I/O problem" (missing or
/// unreadable data, permissions, etc.). Codes `1`/`2` are tracker warning/error and carry
/// `errorString` text controlled by the torrent's tracker, not the local filesystem — matching
/// on those would let a hostile tracker trigger removal of a perfectly healthy torrent by
/// returning "no data found" in a `failure reason`/`warning message` (#483).
pub(super) const TR_STAT_LOCAL_ERROR: i64 = 3;
/// Transmission status for a stopped torrent.
pub(super) const TR_STAT_STOPPED: i64 = 0;

/// Build an `Unknown` `DownloadDataHealthy` condition — used for every non-destructive
/// failure path in [`check_download_client_health`] so a swallowed error still surfaces on
/// the ServarrApp status instead of the condition silently disappearing (#483).
///
/// Takes a [`TenantSafeMessage`] and goes through [`Condition::unknown`] rather than building
/// the struct directly. A direct struct literal would accept a raw `String` and skip the
/// sanitized-message check that `Condition::ok` and `Condition::fail` enforce (#709), which
/// would leave this the one unguarded way to write a tenant-visible Condition message. (#744)
pub(super) fn download_health_unknown(
    reason: &str,
    message: impl Into<TenantSafeMessage>,
    now: &str,
) -> Condition {
    Condition::unknown(condition_types::DOWNLOAD_DATA_HEALTHY, reason, message, now)
}

/// Detection predicate: a torrent reporting a local error, whatever its `percentDone` (#840).
///
/// Match on error code 3 (TR_STAT_LOCAL_ERROR) rather than errorString: Transmission
/// rewrites the message during/after verify (e.g. "no data found" -> "no data was
/// found!"), so substring matching is brittle (#537). A completed torrent whose files were
/// deleted keeps its completion bitfield, so `percentDone` stays `1.0` until a re-verify. A
/// `percentDone` gate here would drop that torrent before verify ever ran.
pub(super) fn is_missing_data_torrent(t: &servarr_api::TorrentInfo) -> bool {
    t.error == Some(TR_STAT_LOCAL_ERROR)
}

/// Removal predicate, checked only after `torrent-verify` settled (#840). A local error also
/// covers I/O problems that leave data on disk (permissions, disk-full, read-only remount).
/// Requiring `percentDone == 0.0` after verify keeps those torrents out of removal.
pub(super) fn is_confirmed_missing(t: &servarr_api::TorrentInfo) -> bool {
    is_missing_data_torrent(t) && t.percent_done == Some(0.0)
}

/// Split torrents into those safe to remediate and the count held back (#840).
///
/// A download directory whose every torrent reports a local error is most likely unmounted,
/// not emptied. Its torrents are held back from verify and removal. A directory needs at least
/// [`MIN_TORRENTS_FOR_DIR_CHECK`] torrents for this test, because one torrent cannot be told
/// apart from lost data.
pub(super) fn partition_unavailable_dirs(
    torrents: Vec<servarr_api::TorrentInfo>,
) -> (Vec<servarr_api::TorrentInfo>, usize) {
    let unavailable = unavailable_dirs(&torrents);
    let (stale, held): (Vec<_>, Vec<_>) = torrents
        .into_iter()
        .filter(is_missing_data_torrent)
        .partition(|t| !unavailable.contains(dir_key(t)));
    (stale, held.len())
}

/// Directory grouping key. A trailing slash is dropped, so `/a` and `/a/` group together (#840).
fn dir_key(t: &servarr_api::TorrentInfo) -> &str {
    t.download_dir.trim_end_matches('/')
}

/// A stopped torrent with no error. It reports no error only because it does not check its data,
/// so it is no evidence that the download directory is mounted (#847). A torrent with a local
/// error is also stopped by Transmission, so the error check keeps it counted.
fn is_idle_stopped(t: &servarr_api::TorrentInfo) -> bool {
    t.status == Some(TR_STAT_STOPPED) && t.error == Some(0)
}

/// Directories where every active torrent reports a local error, with enough torrents to tell a
/// missing mount from lost data. Stopped torrents do not count either way (#847).
fn unavailable_dirs(torrents: &[servarr_api::TorrentInfo]) -> std::collections::HashSet<String> {
    let mut per_dir: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for t in torrents {
        // An omitted downloadDir reads as "" and must not group unrelated torrents together.
        let key = dir_key(t);
        if key.is_empty() || is_idle_stopped(t) {
            continue;
        }
        let counts = per_dir.entry(key).or_default();
        counts.0 += 1;
        if is_missing_data_torrent(t) {
            counts.1 += 1;
        }
    }
    per_dir
        .into_iter()
        .filter(|(_, (total, erroring))| *total >= MIN_TORRENTS_FOR_DIR_CHECK && total == erroring)
        .map(|(dir, _)| dir.to_string())
        .collect()
}

/// Drop torrents without a `hashString`, returning the kept ones and the count dropped (#500).
/// An empty hash would scope `torrent-verify`/`torrent-remove` to an empty id, so skip and warn.
pub(super) fn split_missing_hash(
    candidates: Vec<servarr_api::TorrentInfo>,
) -> (Vec<servarr_api::TorrentInfo>, usize) {
    let (with_hash, missing): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|t| !t.hash_string.is_empty());
    for t in &missing {
        warn!(
            torrent_id = t.id,
            "download-client health: torrent reporting missing data has no hashString, \
             skipping until Transmission reports one"
        );
    }
    (with_hash, missing.len())
}

const MIN_TORRENTS_FOR_DIR_CHECK: usize = 2;

pub(super) fn is_checking(t: &servarr_api::TorrentInfo) -> bool {
    t.status
        .is_some_and(|status| CHECKING_STATUSES.contains(&status))
}

/// Outcome of attempting to remediate a batch of stale torrents, addressed by their stable
/// `hashString` (#500): which were removed as confirmed-orphaned, which were confirmed orphaned
/// but left alone because `auto_remove` is disabled (#498), and which are still hash-checking
/// (retried on the next reconcile).
///
/// `settled_with_data` holds torrents that settled with error 3 but still report data (#845).
/// `settled_unknown` holds torrents that settled with error 3 and no `percentDone`, so their data
/// state is not known (#856).
/// `rpc_failure` names the first Transmission RPC that failed in the pass (#845).
#[derive(Default)]
pub(super) struct RemediationOutcome {
    pub(super) removed: Vec<String>,
    pub(super) confirmed_orphaned: Vec<String>,
    pub(super) still_pending: Vec<String>,
    pub(super) settled_with_data: Vec<String>,
    pub(super) settled_unknown: Vec<String>,
    pub(super) rpc_failure: Option<&'static str>,
}

/// Detect Transmission torrents whose on-disk data has gone missing, trigger a verify so
/// Transmission re-checks, and remove torrents confirmed still broken once the verify
/// settles (#483). Gated behind `apiHealthCheck.enabled` since it needs the same live RPC
/// access as [`check_api_health`]; only applies to Transmission apps. Removal additionally
/// requires `apiHealthCheck.autoRemove` (#498).
pub(super) async fn check_download_client_health(
    app: &ServarrApp,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    transmission_access: Option<&Result<TransmissionAccess, String>>,
) -> Option<Condition> {
    if app.spec.app != AppType::Transmission {
        return None;
    }
    let health_check = match app.spec.api_health_check.as_ref() {
        Some(hc) if hc.enabled => hc,
        _ => return None,
    };
    let now = chrono_now();

    // `intervalSeconds` throttle (#506): while the existing DOWNLOAD_DATA_HEALTHY condition
    // is still inside its poll window, skip the self-heal pass entirely. This is the fix the
    // throttle primarily exists for — a user setting `intervalSeconds: 3600` next to
    // `autoRemoveOrphanedTorrents: true` expects the destructive torrent-remove check at most
    // hourly, not on every reconcile.
    let existing_download = current_condition(app, condition_types::DOWNLOAD_DATA_HEALTHY);
    // Destructive path (torrent-verify / torrent-remove). Throttles on every status, because
    // the pass writes its own time into the condition, so the timestamp records the last
    // attempt (#844). A corrupt timestamp fails closed (#519).
    if is_destructive_poll_throttled(existing_download, health_check.interval_seconds, &now) {
        // A skip is intentional (rate-limit window), but it must be observable — otherwise the
        // self-heal pass pausing (including destructive torrent-remove checks) looks like a
        // hang. This branch returns before the access match, so a broken client/credential
        // surfaced here would otherwise be invisible until the window elapses.
        debug!(
            app = %app.name_any(),
            condition = condition_types::DOWNLOAD_DATA_HEALTHY,
            interval_seconds = health_check
                .interval_seconds
                .unwrap_or(DEFAULT_HEALTH_CHECK_INTERVAL_SECONDS),
            "download client health poll throttled inside intervalSeconds window; keeping existing condition"
        );
        // #519 follow-up: a corrupt/unparseable lastTransitionTime is what triggered the
        // fail-closed skip above, not a genuinely-recent valid one -- re-persisting it
        // verbatim would wedge the destructive path forever, since every future reconcile
        // hits the same unparseable value and fails closed again. Repair it to `now` (keeping
        // status/reason/message) so this reconcile still skips the pass once, but a normal,
        // parseable elapsed-time throttle governs from here on.
        if let Some(existing) = existing_download
            && chrono::DateTime::parse_from_rfc3339(&existing.last_transition_time).is_err()
        {
            warn!(
                app = %app.name_any(),
                condition = condition_types::DOWNLOAD_DATA_HEALTHY,
                last_transition_time = %existing.last_transition_time,
                "download client health: corrupt lastTransitionTime failed closed; repairing \
                 timestamp so the self-heal pass isn't wedged indefinitely"
            );
            return Some(Condition {
                last_transition_time: now,
                ..existing.clone()
            });
        }
        return existing_download.cloned();
    }
    let auto_remove = health_check.auto_remove_orphaned_torrents;

    let access = match transmission_access {
        Some(Ok(a)) => a,
        // `resolve_transmission_access` builds its error string from `log_summary()`, so this
        // is sanitizer output -- category (c) of the TenantSafeMessage::new contract.
        Some(Err(e)) => {
            return Some(download_health_unknown(
                "ClientBuildError",
                TenantSafeMessage::new(e.clone()),
                &now,
            ));
        }
        None => {
            return Some(download_health_unknown(
                "ClientBuildError",
                TenantSafeMessage::new(TRANSMISSION_CLIENT_UNRESOLVED),
                &now,
            ));
        }
    };
    // access.client is built from the same credential read `check_api_health`'s read-only probe
    // uses, but this path can delete torrents, so fail closed instead of silently proceeding
    // unauthenticated when adminCredentials was configured but couldn't be fully read (#483).
    if access.credentials_incomplete {
        warn!(app = %app.name_any(), reason = ?access.credentials_error,
            "download-client health: adminCredentials could not be fully read, refusing to \
             proceed unauthenticated on a destructive path");
        // Name the failure category. A transient or RBAC error is not a problem with the
        // secret contents (#846).
        let message = match &access.credentials_error {
            Some(category) => format!("adminCredentials could not be read: {category}"),
            None => "adminCredentials secret is missing username or password".to_string(),
        };
        return Some(download_health_unknown(
            "CredentialReadError",
            TenantSafeMessage::new(message),
            &now,
        ));
    }
    let tx_client = &access.client;

    let torrents = match tx_client.torrent_get(&TORRENT_HEALTH_FIELDS, None).await {
        Ok(t) => t,
        Err(e) => {
            warn!(app = %app.name_any(), error = %e.log_summary(),
                    "download-client health: torrent-get failed");
            return Some(download_health_unknown(
                "TorrentGetError",
                // Sanitizer output -- category (c) of the TenantSafeMessage::new contract.
                TenantSafeMessage::new(e.log_summary()),
                &now,
            ));
        }
    };

    let (candidates, held_back) = partition_unavailable_dirs(torrents);
    let (stale, missing_hash) = split_missing_hash(candidates);
    if stale.is_empty() && held_back == 0 {
        if missing_hash > 0 {
            // Without a hash the torrent cannot be verified. Reporting "NoStaleData" here
            // would hide a torrent that still reports missing data (#840 review).
            return Some(download_health_unknown(
                "MissingHashString",
                TenantSafeMessage::new(format!(
                    "{missing_hash} torrent(s) reporting missing data have no hashString"
                )),
                &now,
            ));
        }
        return Some(Condition::ok(
            condition_types::DOWNLOAD_DATA_HEALTHY,
            "NoStaleData",
            TenantSafeMessage::new("No torrents reporting missing data"),
            &now,
        ));
    }

    let outcome = remediate::remediate_stale_torrents(tx_client, &stale, auto_remove).await;
    report::report_stale_torrents(recorder, obj_ref, &stale, &outcome, held_back).await;
    let condition = report::build_download_health_condition(stale.len(), held_back, &outcome, &now);
    // A torrent without a hashString is never verified or removed. A True condition would hide it,
    // so the pass reports it even when the other stale torrents recovered (#840 review).
    if missing_hash > 0 && condition.status == "True" {
        return Some(download_health_unknown(
            "MissingHashString",
            TenantSafeMessage::new(format!(
                "{missing_hash} torrent(s) reporting missing data have no hashString"
            )),
            &now,
        ));
    }
    Some(condition)
}
