# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

<!-- next-header -->

## [Unreleased] - ReleaseDate

Fixes instance registration for all three companion apps added in 1.4.0 — Cleanuparr and Houndarr
both failed every registration attempt outright, and the admission webhook rejected any
`spec.appConfig.unpackerr` block before it could even reach a running Unpackerr.

### Fixed

- Fix Cleanuparr instance registration always failing with a 400. `ArrInstanceRequest.Version` is
  required server-side; the operator never sent it (#796).
- Fix Houndarr instance registration always failing. The CSRF token was sent as a `csrf_token`
  form field instead of the `X-CSRF-Token` header — Houndarr's CSRF check consumes the request
  body itself when the header is absent, so every other field came back "missing" (422). The
  create request also needs `connection_verified=true` or the server rejects it outright; Houndarr
  re-probes the *arr instance independently regardless, so this only satisfies a UI-state flag,
  not real validation (#797).
- Fix the admission webhook rejecting any `spec.appConfig.unpackerr` block with a false
  "appConfig variant does not match app type" error — the `Unpackerr` variant was never added to
  `validate_app_config_match`'s match arms (#795).

## [1.4.0] - 2026-09-01

Adds Unpackerr, Cleanuparr, and Houndarr as companion apps, and fixes a `webhook.port` Helm value
that never actually reached the running operator. The bulk of this release fails closed instead of
silently defaulting: every environment-variable read, persistence collision check, and
Kubernetes Event/Condition message now either surfaces a real problem or gets sanitized, rather
than swallowing it, substituting an unnoticed default, or leaking upstream error detail. Also fixes
a CRD schema bug that silently pruned the legacy `overseerrSync` field, and bumps default images
across the board — including SABnzbd to 5.1.1, which closes a critical authentication-bypass
vulnerability in 5.1.0 and earlier.

### Added

- Add Unpackerr, Cleanuparr, and Houndarr as companion apps. Unpackerr extracts archives for
  Sonarr/Radarr/Lidarr/Readarr after download; it has no live API, so an init container seeds
  `/config/unpackerr.conf` once, at pod creation, from `spec.appConfig.unpackerr`. Cleanuparr
  removes stalled/blocked/malicious downloads and triggers re-searches; set `cleanuparrSync.enabled`
  to auto-register discovered Sonarr/Radarr instances via its JSON API (reuses `apiKeySecret`, same
  as Maintainerr). Houndarr runs rate-limited missing/cutoff/upgrade searches; it has no
  registration API of its own, so `houndarrSync` logs in with `adminCredentialsSecret` and drives
  its settings form directly — a stopgap until Houndarr gains a proper API (#775). Cleanuparr and
  Houndarr share the same idempotent list-then-register sync logic Maintainerr already used for
  Sonarr/Radarr, now extracted into one shared code path. New `CleanuparrSyncReady` and
  `HoundarrSyncReady` status conditions report each sync's health (#604, #605, #606).
- Add a `webhook.port` Helm value (default `9443`). The Service hardcoded `targetPort: 9443` and
  the Deployment hardcoded `containerPort: 9443`, so `WEBHOOK_PORT` was never actually usable —
  setting it moved the operator's listener and left the API server dialing a port nothing was on.
  The one value now drives the container port, the Service target, and the operator's own
  `WEBHOOK_PORT` together (#732).

### Image Updates

- **SABnzbd**: `4.5.5` -> `5.1.1`
  - Fixes a critical authentication-bypass vulnerability (GHSA-xrfq-jhgh-wqch) in 5.1.0 and
    earlier: an attacker who can reach SABnzbd's own web login could obtain a valid session even
    behind a username/password, exposing everything in SABnzbd. You're affected only if that web
    interface is reachable by an untrusted party — check `External internet access` is not set
    beyond its default `No access`. If you were exposed, rotate the SABnzbd username/password,
    its API key, and any Usenet/indexer/notification credentials stored in it.
  - 5.0.0 adds NNTP Pipelining and Direct Write (both opt-in for existing servers; new servers
    default to pipelining) and reworks the article cache; upgrading from before SABnzbd 3.0.0
    requires a manual queue repair.
- **Tautulli**: `2.17.2` -> `2.18.1`
  - Fixes a path-traversal bug in update-tarfile extraction and a Plex-token leak via a
    case-sensitive URL bypass in `pms_image_proxy` (both in 2.18.0), and a guest-user API
    permission bug that let a guest read another user's history (2.18.1).
  - Adds anonymous system-analytics telemetry, with an opt-out toggle in the setup wizard and
    settings page (2.18.0).
- **Maintainerr**: `3.15.2` -> `3.26.0`
  - Adds anonymous weekly telemetry; opt out with the `TELEMETRY=off` environment variable
    (3.25.0).
  - Hardens the appliance against cross-origin reads, unvalidated settings bodies, HTML in
    email, and world-writable code (3.22.0).
  - Adds Sportarr as a native application connection and metadata provider (3.20.0, 3.26.0).
- **Jackett**: `0.24.2424` -> `0.24.2486` (indexer-definition rollups)

### Fixed

- Fix `scripts/smoke-test-local.sh` leaving a developer's kubectl context pointed at a deleted
  namespace after every run, instead of restoring the namespace it had before the script ran.
  The script also now refuses to run at all against a cluster it can't identify as a local one
  (`kind`/`k3d`/`docker-desktop`/`rancher-desktop`), rather than proceeding on an unrecognized
  cluster that could be shared or production (#762).
- Fix an app's startup probe giving up too early when many apps deploy at once. Deploying a full
  MediaStack briefly saturates a modest node's CPU while every container starts, which can stretch
  a slow-starting app's own startup work well past the previous 300s budget — kubelet then kills
  and restarts the container before it gets a real chance. Raised the budget to 600s (#764).
- Fix every environment-variable read in the operator treating "you never set this" and "you set
  this to something I can't read" as the same thing — a value that isn't valid UTF-8 was silently
  discarded and the built-in default applied with no log line at all. A genuinely unset variable
  logs at `debug!`. An unreadable one now either warns and names the variable, or stops the
  operator outright where a default would change a security or availability posture (see below).
  Covers `WATCH_ALL_NAMESPACES`, `WATCH_NAMESPACE`, `POD_NAME`, `WEBHOOK_ENABLED`, `WEBHOOK_PORT`,
  the `WEBHOOK_TLS_*` paths, and the `DEFAULT_IMAGE_*` overrides (#725, #726, #730, #732).
- Refuse to start when a webhook or scoping variable is set to a value the operator can't use,
  rather than quietly substituting a default. That's `WEBHOOK_PORT` (unparseable, or `0`),
  `WEBHOOK_ENABLED` (anything outside `true`/`false`/`1`/`0`/`yes`/`no` — `on` and `y` used to
  mean `false`), an empty `WEBHOOK_TLS_DIR`/`WEBHOOK_TLS_CERT`/`WEBHOOK_TLS_KEY`, and an
  unreadable `WATCH_NAMESPACE`. The reason a fallback was wrong for these is that it *worked*: the
  chart mounts a valid cert at the default path, so an unreadable `WEBHOOK_TLS_CERT` came up
  healthy serving a certificate nobody picked, and an unreadable `WATCH_NAMESPACE` widened the
  operator from one namespace to the whole cluster. `WATCH_ALL_NAMESPACES` and `POD_NAME` keep the
  old lenient behavior on purpose — `false` is the narrower scope, so its default is already
  fail-safe (#732, #733, #739).
- Fix a webhook that can't start leaving the operator running and reporting itself healthy. A
  missing cert file, an unparseable PEM, a cert/key mismatch, a denied read on the mounted secret,
  or a port already in use all reduced to one `error!` line while `/readyz` kept returning 200 —
  and with `failurePolicy: Fail`, every `ServarrApp` write in the cluster failed. It now takes the
  operator down with it, same as the metrics server already did (#733).
- Read the `WEBHOOK_TLS_*` paths as `OsString` instead of UTF-8 text, so a filesystem path that
  isn't valid UTF-8 just works rather than being discarded (#733).
- Fix an unusable `RUST_LOG` being discarded in silence — a malformed filter directive (or a
  non-UTF-8 value) dropped the operator to its default `servarr_operator=info,kube=info` filter
  with nothing to say so. It now warns and names the parse error. The Helm chart always sets
  `RUST_LOG`, so this fired on a real, user-edited value rather than a hypothetical one (#731).
- Fix an empty `WATCH_NAMESPACE` silently widening the operator from one namespace to the whole
  cluster. The existing `cluster-scoped mode` line never said why it chose that; an empty value
  now warns on its own (#731).
- Reject `WEBHOOK_PORT=0` instead of binding an OS-assigned random port while the Service keeps
  routing to the configured one — with `failurePolicy: Fail` on the validating webhook, that
  combination failed every `ServarrApp` write in the cluster, and the only clue was a startup
  line reading `0.0.0.0:0`. Both `0` and an unparseable value now stop the operator (#731, #732).
- Fix an unreadable `DEFAULT_IMAGE_<APP>_TAG` or `DEFAULT_IMAGE_<APP>_REPO` producing an image
  nobody asked for. `DEFAULT_IMAGE_SONARR_REPO=myrepo/sonarr` plus a `_TAG` the operator couldn't
  read gave you `myrepo/sonarr:<whatever tag the operator ships>` — a pairing that may not exist
  in the registry, or may exist and be the wrong build. An unreadable half now drops that app's
  whole override, so the matching built-in default applies instead of a mix. An *unset* tag is
  unchanged: the chart renders `value: ""` when you override only `repository`, and the built-in
  tag filling that in is the point (#38, #734).
- Fix the image-override startup line printing the raw override rather than the image that will
  actually be pulled — an override setting only `repository` logged an empty tag, so you couldn't
  tell from the logs which image you were getting. It now logs the merged result (#734).
- Fix any app's default image silently sticking on a `helm upgrade --reuse-values` upgrade —
  generalizes the 1.3.1 Seerr-only stale-image detection into a `StaleDefaultImage` Warning Event
  that fires for every app whose env-supplied default image no longer matches the running
  operator's own built-in default (#638). `docs/installation.md` now recommends
  `--reset-then-reuse-values` (Helm 3.14+) over `--reuse-values` for upgrades.
- Fix a stuck `MediaStack` orphan's `OrphanCleanupHealthy` condition reporting "may
  self-resolve" for a PVC-detach failure that never will — a `401`/`403` (expired credential,
  missing RBAC) is now labeled separately from a genuinely transient `5xx`/network failure, so
  on-call doesn't wait on a retry that was never going to clear it (#610).
- Fix a drift correction failing outright when the advisory `DriftDetected` Event couldn't be
  published (an `events.k8s.io` RBAC hiccup) — it's now logged and counted instead of aborting an
  otherwise-successful reconcile (#646).
- Fix several `ServarrApp` reconcile Events (`ReconcileError`, `BackupStarted`,
  `BackupCompleted`, `BackupFailed`, and others) silently discarding a publish failure with no
  log line — all now warn and increment a new `servarr_operator_event_publish_failures_total`
  metric, so persistent Events-API breakage is visible instead of only missing Events (#646).
- Reject a literal `..` segment in a persistence `mountPath` outright, instead of silently
  accepting one that reaches the pod spec as a non-canonical string when it doesn't collide with
  anything (#487).
- Fix the reserved-mount collision error to always name the `mountPath` the user actually wrote,
  instead of sometimes showing the reserved path's own literal (#712).
- Fix a stuck orphan's PVC ownerReference never getting detached when `spec.persistence` has a
  mount-path collision the webhook missed — `cleanup_orphaned_children` now falls back to a
  namespace-wide PVC lookup scoped by ownerReference (never by label, so it can't touch a PVC it
  doesn't own) instead of giving up, so the child's PVC — and any later stack delete — stays
  protected even when the spec can't be resolved (#673, #719).
- Fix the admission webhook silently admitting a `ServarrApp`/`MediaStack` with zero persistence
  validation when its app type had no compiled defaults — it now rejects the object instead
  (#716).
- Fix the admission webhook silently admitting an UPDATE whose stored spec fails to parse (or is
  missing entirely) — `validate_identity_immutable` now rejects the object instead of skipping
  the `spec.app`/`spec.instance` immutability check (#720).
- Fix `MediaStack` child-readiness read-back treating a real API/network error the same as "not
  ready yet" — a failed status read now warns with the underlying error instead of silently
  falling through to `false` (#721).
- Fix a stuck orphan's failed child delete (after a successful PVC detach) being visible only in
  operator logs — it now surfaces via the `OrphanCleanupHealthy` status condition like the other
  failure buckets (#722).
- Fix the version number in the `UpdateAvailable` condition being copied straight out of the app's
  update API with no length limit and no character filter. Two things went wrong with that. A long
  enough value pushed the condition message past the 32768 bytes Kubernetes allows, so the status
  patch was rejected and the app reconciled in a loop; and whatever the app returned was shown to
  you verbatim. The version is now filtered to the characters a version number actually uses and
  capped, and a value that survives neither is reported as "A new version is available" rather than
  costing you the whole condition (#744).
- Fix `servarr_operator_event_publish_failures_total` not counting the two events the finalizer
  cleanup path publishes. One of those sites discarded its publish failure outright and the other
  only logged it, so an `events.k8s.io` outage during cleanup showed up in neither the metric nor,
  for the first site, anywhere at all (#708).
- Fix the operator logging that it failed to publish an event without saying which app it was for.
  None of these log lines carried the object name, so on a cluster-wide operator the line named a
  reason and nothing else (#744).
- Fix an `events.k8s.io` hiccup failing a reconcile that had already succeeded. The terminal
  `ReconcileSuccess` event was the one event this operator published without going through the
  shared helper, and a failed publish returned an error — so an otherwise-complete reconcile took
  the error-backoff requeue path and repeated real work for the sake of one informational write.
  Three signals disagreed about the same reconcile, too: the success counter had already recorded
  a success, the reconcile reported a failure, and the publish-failure metric stayed at zero
  because that site never incremented it. The publish is now advisory like every other event —
  it warns, names the app, and counts the failure under
  `servarr_operator_event_publish_failures_total{reason="ReconcileSuccess"}`. If you alert on
  reconcile errors, Events-API blips will stop showing up there; watch that metric instead (#746).
- Fix a long event note costing you the whole event. `events.k8s.io/v1` rejects a note over 1024
  bytes, so the publish failed outright rather than delivering a shortened message. Individual
  call sites capped their own input, which left every new site to remember the limit on its own.
  The cap now lives in the one function every note passes through, and it cuts on a character
  boundary so a multibyte character can't be split (#747).
- Fix a failed ServarrApp list silently leaving `servarr_operator_managed_apps` stuck at its last
  value — an RBAC change or an API server under pressure now warns and counts under a new
  `servarr_operator_managed_apps_list_failures_total` metric instead of a gauge that looks
  current forever (#751).
- Fix `error_policy`'s `ReconcileError` event publish running in a detached task with the handle
  dropped, so a shutdown before that task got polled skipped the publish, and with it the
  failure metric that lives inside it. The publish is now tracked and drained on shutdown
  instead (#752).
- Fix the same stale-gauge bug in `servarr_operator_managed_stacks`, found while fixing #751 — a
  failed MediaStack list now warns and counts under a new
  `servarr_operator_managed_stacks_list_failures_total` metric (#753).
- Fix backup-retention pruning silently skipping when listing existing backups failed, letting
  old backups accumulate past retention with nothing to say pruning didn't run. It now warns and
  counts under `servarr_operator_backup_operations_total{operation="prune",result="error"}` (#753).
- Fix #752's shutdown drain only running when `controller::run` happened to win the race in
  `main.rs`'s `tokio::select!` against the webhook server, the metrics server, and
  `media_stack_controller::run`. On a normal `SIGTERM` both controllers wind down around the same
  time, so whichever future resolved first cancelled the others mid-poll — including a drain still
  waiting on an in-flight `ReconcileError` Event publish. The drain now runs once, process-wide,
  after the `select!` resolves regardless of which branch won, bounded by a 5s timeout so a hung
  publish can't block shutdown past `terminationGracePeriodSeconds`. A publish abandoned at that
  timeout now also counts under `servarr_operator_event_publish_failures_total{reason="ShutdownTimeout"}`
  instead of only logging (#755).
- Fix Kubernetes silently dropping a manifest that still spells the legacy `overseerrSync` field,
  instead of accepting or rejecting it. The field-level alias existed in Rust
  (`#[serde(alias = "overseerrSync")]`) but never reached the generated CRD schema — schemars
  can't add a sibling property key for an alias on an object-typed field the way it does for the
  `AppType` enum alias, so the CRD only ever declared `seerrSync`. Kubernetes' structural schema
  then silently pruned any `overseerrSync` a manifest still used, which is worse than a loud
  rejection. Both `ServarrApp`'s and `MediaStack`'s generated CRD schemas now declare
  `overseerrSync` alongside `seerrSync` (#545). Also corrects a stale `docs/troubleshooting.md`
  claim that the operator auto-registers the CRD on startup — it only performs a read-only
  staleness check (#563).
- Fix the Prowlarr/Seerr finalizer-cleanup path logging every non-404 failure identically,
  giving on-call no way to tell a permission/config problem that needs a manual fix from one
  that will clear on its own without reading pod logs. A 401/403 or credential failure is now
  logged separately from a genuinely transient one; the tenant-visible `CleanupFailed` Event is
  unchanged, since revealing which category fired would itself leak operator control-plane state
  (#669, #674). Also adds a debug-level log line distinguishing a PVC 404 because it was already
  detached from one because the computed name never matched a real PVC (#670).

### Changed

- CI now enforces a per-file coverage floor alongside the aggregate one. An aggregate threshold
  passes happily while one module sits far below the line and another sits near 100%, so
  `.coverage-floors` records a floor per file and `scripts/check-coverage.sh` fails the build when
  any file drops below its own. A file with no entry uses a default floor, so a new module added
  without tests cannot slip through. Both gates read a single `cargo llvm-cov` JSON report, so
  coverage is still measured once per CI run (#643).
- The coverage step in the branch CI workflow no longer pipes its result through `tee`. That pipe
  ran without `pipefail`, so it masked the exit code — the same defect #70 fixed once already, in
  the same place. The gate's exit status is now the step's status (#643).
- Raised the workspace coverage threshold from 86% to 90%. Line coverage currently measures
  91.25%. `controller.rs`, the module that motivated the original 75% floor, is at 87.73% —
  it was 43.21% when that issue was written (#71).
- `WEBHOOK_ENABLED` now accepts the same values `WATCH_ALL_NAMESPACES` already did — `true`/`false`,
  `1`/`0`, and `yes`/`no`, each case-insensitively — and warns on anything else. It previously
  matched only the exact strings `true` and `1`, so `WEBHOOK_ENABLED=TRUE` silently left the
  admission webhook switched off. Such a value now switches it on. The default when the variable is
  unset is still `false` (#730).
- `Condition::fail` now requires a sanitized message type instead of a plain string, closing a
  gap where a future code change could have leaked raw API-server error detail into a
  tenant-visible status Condition. No existing Condition message changes as a result (#668).
- `Condition::ok` and `Condition::unknown` now also require that same sanitized message type —
  the last two ways to build a tenant-visible Condition without going through it. No existing
  Condition message changes as a result (#709).
- `EnvError`'s `reason` field is now `&'static str` instead of `impl Into<String>`, so the type
  system enforces the "never log the value, only the variable name and length" rule the doc
  comment already stated. Every current call site was already clean, so this only closes the door
  on a future one interpolating a credential-bearing value into a log line (#740).
- `Context`'s fields (`client`, `image_overrides`, `legacy_image_override_apps`, `reporter`,
  `watch_namespace`, `app_api_base_override`, `event_publish_tasks`) are now `pub(crate)` instead
  of `pub`. Any code with crate access could previously build a `Context` via struct literal and
  skip the `WATCH_NAMESPACE` validation `Context::new` performs, silently widening the operator
  from namespace-scoped to cluster-scoped with no startup log line (#757).
- Persistence mount-path collision detection now resolves known base-image symlink aliases
  (`/var/run` -> `/run`, `/var/lock` -> `/run/lock`) before comparing, so an override spelled
  through the symlinked form now correctly collides with the reserved real path — a spec that
  previously applied cleanly through this gap is now rejected (#484).
- Add a volume-*name* collision guard alongside the existing mount-path one — a user-supplied
  volume name colliding with an operator-reserved name, or via the `nfs-<name>` prefix an NFS
  mount gets in the actual pod spec, is now rejected (#485).
- All persistence collision checks (mount-path, volume-name, and the `..` rejection above) now
  also run at CRD admission time via the validating webhook, instead of only at reconcile — a
  colliding spec is rejected at `kubectl apply` time instead of only failing on the next
  reconcile (#486).
- Kubernetes Event notes now require the same sanitized message type status Conditions already
  did, closing the gap #668 closed for Conditions. Event notes are tenant-visible through
  `kubectl get events` and took a plain string, so nothing stopped a future code change from
  interpolating a raw API-server error or an upstream response body into one. No existing event
  note changes as a result (#747).

## [1.3.1] - 2026-08-27

Closes out the Overseerr → Seerr migration started in 1.3.0 — the CRD schema now accepts the
legacy `overseerrSync` field spelling, not just the `app: Overseerr` enum value, and stale
image/auth fallbacks now surface as Warning Events instead of only a log line; hardens
Transmission's download-data self-heal against false-positive removals and oversized Events; makes
MediaStack's orphan cleanup safe against a stuck PVC detach, with the stuck state now visible on
status; and adds a startup check that warns when installed CRDs lag the running operator build.

### Added

- Add `docs/upgrade-1.3.md`, a full 1.2.x → 1.3.x migration guide covering the Overseerr→Seerr
  rename, the legacy `appConfig.transmission.auth` conflict, `apiKeySecret` requirements for
  `apiHealthCheck`, and the Transmission download-data self-heal opt-ins.
- Add a startup check that warns when the installed CRDs are missing a field the running operator
  build expects, catching a missed `servarr-crds` upgrade before it silently drops config (#543).

### Image Updates

- **Transmission**: `4.1.2` -> `4.1.3`
- **Jackett**: `0.24.2304` -> `0.24.2424` (indexer-definition rollups)

### Fixed

- Fix `MediaStack` reconcile wedging permanently in an error loop when an app entry renamed from
  `Overseerr` to `Seerr` — the old child `ServarrApp` was deleted only after trying to apply the
  new one, which the admission webhook rejected as a duplicate instance (#533).
- Fix a stale `DEFAULT_IMAGE_OVERSEERR_*` env var (e.g. from a Helm release upgraded with
  `--reuse-values`) silently driving a `Seerr` app's image with only a startup log line to notice
  — the operator now publishes a `DeprecatedImageOverride` Warning Event when the fallback is
  actually in effect (#534).
- Fix `Seerr` apps unable to write their config after an Overseerr→Seerr migration, because the
  inherited config volume stayed owned by the old app's uid/gid and Seerr runs as a fixed,
  non-configurable `1000:1000` — the operator now migrates ownership automatically on first
  reconcile after the transition (#535).
- Fix `kubectl apply` of a pre-1.3 manifest spelled `app: Overseerr` being rejected outright by the
  CRD's structural schema, even though already-stored objects with that spelling kept reconciling
  fine — the CRD schema now accepts the legacy value too (#540).
- Fix `Transmission` reconcile failing outright (SSA 500, duplicate `USER`/`PASS` env vars) when
  both `spec.adminCredentials` and the legacy `spec.appConfig.transmission.auth` were set —
  `adminCredentials` now wins and the legacy block is skipped instead of conflicting (#536).
- Fix the Transmission download-data self-heal losing track of orphaned torrents after
  Transmission rewrote its own error message during verify (`"no data found"` -> `"no data was
  found"`), which silently escaped the removal predicate — now matches on the error code instead
  of the message text, and additionally requires `percentDone == 0.0` so a transient local I/O
  fault (permissions, full disk, read-only remount) sharing the same error code can't trigger
  torrent removal on an otherwise-healthy session (#537).
- Fix the `DownloadDataMissing` event never publishing for large stale-torrent batches because its
  note exceeded the Kubernetes Events API's 1024-character limit — the torrent-id list is now
  truncated (#538).
- Fix a spurious root-user warning logged on every reconcile for apps that intentionally run as
  root by design (e.g. `SshBastion`) — the warning now only fires when running as root deviates
  from the app's own default (#539).
- Fix the legacy `appConfig.transmission.auth` config being silently dropped with no signal —
  the operator now publishes a `DeprecatedTransmissionAuth` Warning Event and log warning so
  users upgrading from 1.2 know to remove it (#542).
- Fix orphan cleanup deleting a renamed/removed app's `ServarrApp` CR even when detaching its
  PVC's ownership failed first, letting Kubernetes' cascading garbage collection destroy the
  still-owned config PVC along with it — the delete is now skipped and retried on the next
  reconcile until detach fully succeeds, and a new `OrphanCleanupHealthy` status condition
  surfaces a stuck orphan instead of leaving it visible only in pod logs (#562).
- Fix `kubectl apply` of a manifest still spelled `overseerrSync` being silently pruned by the
  CRD's structural schema instead of rejected or accepted — unlike the `app: Overseerr` enum
  value (#540), the field's legacy alias wasn't reflected into the generated schema at all, so
  the config silently vanished on apply with no error. The CRD schema now accepts both spellings
  for both the `ServarrApp` and `MediaStack` CRDs (#545).
- Fix `docs/troubleshooting.md` incorrectly claiming the operator auto-registers the CRD on
  startup and recommending `create`/`patch` RBAC verbs for it — the operator only ever performs
  a read-only startup staleness check (#543), scoped to `get` on the two CRDs it uses (#563).

## [1.3.0] - 2026-08-07

Migrates Overseerr support to its successor, Seerr; adds Transmission download-data self-heal and
fixes the `apiHealthCheck.intervalSeconds` throttle; hardens persistence (a way to intentionally
drop a default volume, stronger mount-path collision detection); and sweeps the controller/webhook
to stop leaking raw upstream/Kubernetes errors into tenant-visible status and Events.

### Added

- Add `persistence.removedDefaultVolumes` to `ServarrApp` and `MediaStack`. The automatic
  restore added for #367 always brings back a default volume a persistence override drops —
  this field lets you say "I mean to drop this one" instead. Entries must name an actual
  default volume for the app type; a typo is rejected at admission rather than silently
  no-opping (#386).
- Add detection and self-healing for Transmission torrents whose on-disk data has gone missing
  (e.g. an external cleanup job deleted files a torrent still references). When
  `apiHealthCheck.enabled` is set on a `Transmission` app, the operator now detects affected
  torrents, triggers Transmission's own re-verify, and removes torrents confirmed still broken
  once that verify settles — reported via a `DownloadDataMissing` Event and a new
  `DownloadDataHealthy` status condition (#483).

### Changed

- Two persistence entries mounting at the same path now fail the reconcile with a clear error
  naming both volumes and the colliding path, instead of producing an invalid pod spec the API
  server silently rejects (#386).
- `ReconcileError` Kubernetes Events for app-configuration problems (e.g. the mount-path
  collision above) now include the actual cause instead of a generic "Application
  configuration error" — the cause is always derived from your own spec, so nothing internal
  leaks (#386).
- The mount-path collision check above now also catches a persistence override colliding with
  a mount the operator injects itself (Transmission's `/watch` dir and admin-credentials
  script, Prowlarr's custom-indexer-definitions dir, SSH bastion's `authorized-keys` mount),
  not just other persistence entries, and normalizes trailing/doubled slashes so
  `/downloads//` is caught the same as `/downloads/` (#402).
- The mount-path collision check now also collapses `..` segments, so a persistence override
  like `mountPath: /watch/foo/../../watch` is recognized as the same real path as the reserved
  `/watch` instead of slipping past the check as a "different" path. A spec relying on this gap
  to shadow an operator-reserved mount now correctly fails the reconcile instead of silently
  producing two `volumeMounts` at the same location (#465).
- Update default Overseerr image `linuxserver/overseerr:1.35.0` -> `ghcr.io/seerr-team/seerr:v3.4.1`
  (repository moved). Overseerr is unmaintained (upstream archived 2026-02-15); its team merged
  with Jellyseerr to form **Seerr**, the actively maintained successor. `AppType::Overseerr` is
  renamed to `AppType::Seerr` — existing `ServarrApp`/`MediaStack` CRs spelled `app: Overseerr` (or
  `overseerrSync`) keep reconciling via a backward-compat alias, but a fresh `kubectl apply` of an
  old manifest still spelled that way will be rejected: the generated CRD schema's `enum` list only
  includes `Seerr`, since `schemars` doesn't propagate serde aliases into the schema it emits.
  Update `app: Overseerr` to `app: Seerr` (and `overseerrSync` to `seerrSync`) in any manifest you
  re-apply. Existing deployments upgrade in place with no data migration needed: Seerr detects an
  inherited Overseerr database on first boot and migrates it automatically, and the operator now
  mounts the app's `config` volume at `/app/config` (was `/config`) and runs it as a fixed UID/GID
  1000 (Seerr's image runs as a non-configurable `node` user, unlike the LinuxServer image it
  replaces). Take a PVC snapshot before upgrading an existing Overseerr app, per the usual guidance
  for any operator-managed change that replaces a running deployment — see `docs/backup-restore.md`.
  The `OverseerrSyncReady` status condition and `OverseerrSync`/`OverseerrCleanup` Event reasons are
  renamed to `SeerrSyncReady`/`SeerrSync`/`SeerrCleanup` — update any `kubectl wait
  --for=condition=...` gate or event-reason alert that references the old names. Because
  `Deployment.spec.selector` is immutable and its `app.kubernetes.io/name` label value changes with
  the rename, the operator recreates (not patches) an existing Overseerr app's Deployment the first
  time it reconciles after upgrade — a one-time pod restart, not a crash loop. If you set a Helm
  `defaultImages.overseerr` override, rename it to `defaultImages.seerr` — the operator still reads
  the old `DEFAULT_IMAGE_OVERSEERR_REPO`/`_TAG` env vars as a fallback (with a startup warning) so
  the override doesn't silently stop applying, but that fallback is not permanent (#44).
- Reduce Transmission health-check overhead: the operator now resolves the adminCredentials
  secret and builds the Transmission RPC client once per reconcile instead of once per health
  check, halving Secret reads and session-ID handshakes for Transmission apps with both the
  general API health check and the download-client self-heal check enabled (#499).
- Enforce `apiHealthCheck.intervalSeconds`, which was documented but never read: a healthy
  (`True`) health condition is re-polled only after the interval elapses, while error, `Unknown`,
  and `False` conditions re-poll immediately and `intervalSeconds: 0` probes on every reconcile
  (#506).
- Drop the unused `apiKeySecret` requirement from the Transmission API health check: the probe
  now authenticates with `adminCredentials` when configured, so `app: Transmission` CRs no longer
  need an `apiKeySecret` just to enable health checking or the download-data self-heal (#509).
- Clarify that `apiHealthCheck.intervalSeconds` does not bound the Transmission
  admin-credentials sync RPC: that sync runs on every reconcile regardless of the interval, by
  design — Transmission's LSIO container image resets RPC authentication on every restart, and
  the sync is the only mechanism that re-enables it, so it must not wait out an interval after a
  restart (#517).

### Removed

- Remove the half-working Lidarr YouTube Downloader sidecar (no upstream image we control, superseded by yt-dlp download support). `spec.appConfig.lidarr.youtubeDownloader` and its `LidarrYoutubeDownloaderSpec` are gone from the CRD — with structural pruning the field is silently dropped on apply, so any `ServarrApp` still setting it simply stops getting the sidecar on the next reconcile (#362).

### Fixed

- Fix half-set Transmission credentials silently producing an unauthenticated client:
  `TransmissionClient::new` now errors when only one of username/password is present, and the
  health-check path reuses the resolved adminCredentials client instead of falling back to a
  partially authenticated one on the destructive download-health pass (#505, #508).
- Fix status conditions always reporting HTTP status 0 for Sonarr/Radarr/Lidarr/Prowlarr/
  Overseerr API errors instead of the real upstream status (#406).
- Fix ~60 call sites across the controller writing raw Kubernetes API errors, secret-read
  errors, or upstream API errors straight into status conditions and Events instead of a
  sanitized summary (#407).
- Fix the admission webhook echoing the raw Kubernetes API-server error (RBAC denial text,
  service-account names) back to `kubectl apply` when its duplicate-instance check fails —
  the rejection message now carries only the HTTP status code (#422). Two log-only
  cleanup-failure warnings in the media-stack controller were sanitized the same way (#423).
- Fix HTTPRoute, TCPRoute, and Certificate builders silently treating a resource-construction
  bug as "not configured" instead of surfacing it as an error (#399).
- Fix SSH bastion host key changing after a persistence override (e.g. a MediaStack's
  stack-wide `persistence.volumes` applied to a bastion with no per-app override) silently
  dropped the `host-keys` PVC volume, causing the bastion to generate a fresh SSH host key
  on next deploy and breaking every client's `known_hosts` trust (#305). This protection is
  now generalized to every app type's default persistence volumes, not just SshBastion's
  `host-keys` — a persistence override can no longer silently drop Subgen's `models` volume,
  the `downloads` volume on Sonarr/Radarr/Lidarr/SABnzbd/Transmission, or Maintainerr's
  relocated `config` volume (#367).
- Fix the Deployment, Service, NetworkPolicy, PVC, HTTPRoute, and TCPRoute builders silently
  applying an empty or invalid resource — or, for HTTPRoute/TCPRoute, panicking — instead of
  failing the reconcile loudly when an app's compiled defaults fail to load (#368).
- Fix a Kubernetes Event publish failure (RBAC restriction, API server unavailable, namespace
  being torn down) being silently discarded instead of logged, across every Event the operator
  publishes (#403).
- Fix `persistence.removedDefaultVolumes` not deduplicating a name listed twice within a
  single layer when merging a MediaStack's stack-wide persistence with a member app's own
  override (#401).
- Emit a Kubernetes Warning Event (reason `CleanupFailed`) when finalizer cleanup of a deleted
  Prowlarr or Overseerr registration fails, instead of leaving the failure visible only in
  operator logs — the Event carries a tenant-safe summary of the failure, and no Event is
  published on success or when there is nothing to clean up (#444).
- Fix the `apiHealthCheck.intervalSeconds` throttle failing open (running the poll) when an
  existing status condition's `lastTransitionTime` was missing or unparseable. That was safe for
  the read-only API health probe, but on the destructive Transmission download-client self-heal
  pass (torrent-verify, and torrent-remove when `autoRemoveOrphanedTorrents` is set) a corrupt
  timestamp meant the self-heal ran unthrottled on every reconcile instead of at most once per
  `intervalSeconds`. The self-heal pass now fails closed on a corrupt timestamp — treating it as
  throttled and repairing the timestamp so it self-heals on a later reconcile rather than being
  wedged indefinitely — while the read-only health probe keeps its existing lenient behavior
  (#519).
- Fix Prowlarr/Overseerr finalizer cleanup silently giving up on a transient failure (e.g. a
  Kubernetes API hiccup listing ServarrApps) instead of retrying — the finalizer was dropped and
  the CR deleted regardless of outcome, permanently orphaning the downstream registration with no
  way to notice. Cleanup failures are now classified as terminal (the registration is provably
  already gone — treated as success) or transient (finalizer kept, cleanup retried) (#451).
- Fix Transmission self-heal removing a torrent confirmed to have missing on-disk data
  whenever `apiHealthCheck.enabled` was set, with no separate consent for the destructive
  removal step — an app that already had the flag enabled before self-heal shipped would start
  deleting torrent entries on its next reconcile after upgrading. Removal now additionally
  requires `apiHealthCheck.autoRemoveOrphanedTorrents: true`, which defaults to `false`;
  detection, the `DownloadDataMissing` Event, and the `DownloadDataHealthy` condition still
  fire under `enabled` alone (#498).
- Fix Transmission self-heal addressing torrents by their process-local numeric id, which
  Transmission reassigns from a per-process counter that resets on daemon restart. A restart
  landing inside the ~1s detect-to-remediate window could make `torrent-verify`/`torrent-remove`
  act on a different torrent than the one detected. The operator now addresses torrents by
  their stable content hash instead (#500).

### Security

- Fix several Kubernetes Events and status conditions (`AdminCredentialsConfigured`,
  `AppHealthy`, backup/restore results, `BazarrSyncReady`, `MaintainerrSyncReady`) including
  raw upstream API error text, including the response body, instead of a sanitized summary.
  For admin-credential sync in particular, an upstream app that echoed a rejected request body
  back could have put the plaintext admin password into `.status.conditions[].message`,
  readable by anyone with `get servarrapps` in the namespace (#398).
- Mark the Transmission, Sonarr/Radarr/Lidarr/Prowlarr, and Maintainerr API clients' credential
  headers (`Authorization: Basic ...`, `X-Api-Key`) as sensitive on the underlying HTTP client,
  so they're redacted rather than printed in full if a client is ever `Debug`-formatted (e.g. an
  errant `tracing::debug!(?client, ...)`) — closes the gap before any such call site exists.
- Fix the Transmission self-heal credential fail-closed gate only catching a *partial*
  adminCredentials read failure (one of username/password unreadable), not a *total* one (both
  unreadable — e.g. the secret was deleted or renamed) — a total failure could proceed
  unauthenticated on the destructive torrent-remove path instead of failing closed like the
  partial case already did.
- Fix admin credentials and API keys leaking into `status.conditions[]` and Kubernetes Events
  when Sabnzbd or Tautulli returned a transport error (connection refused, timeout, etc.)
  during credential setup — both apps send credentials as URL query parameters, and the
  error-sanitization helper wasn't stripping the request URL from that specific error variant
  (#421).
- Fix `error_policy` publishing raw Kubernetes API error text (RBAC denial detail, exec-auth
  credential-plugin failures, kubeconfig paths) into the tenant-visible reconciliation-failure
  Warning Event and several status Condition messages instead of a sanitized summary. The
  admission webhook's duplicate-instance rejection message — the one other tenant-facing caller
  of the sanitizer — is now routed through a stricter summary that never passes through the
  underlying error's raw text, closing a gap where a non-API-server failure (auth, transport,
  kubeconfig) could still leak past the #422 fix (#428, #429, #430).
- Fix ~31 remaining call sites across backup/restore, Prowlarr/Overseerr/Bazarr/Maintainerr sync,
  and Subgen-Jellyfin sync using the log-only error summary (safe for logs, not for tenants)
  instead of the stricter tenant-safe one wherever the result reaches a status Condition or
  `status.backupStatus.lastBackupResult`, and several places interpolating an upstream API
  client error's raw text instead of its sanitized summary — same failure class as #428/#429/#430,
  found while sweeping the rest of the controller for the same pattern (#437, #438).
- Make the tenant-safe guarantee compile-time enforced instead of convention: the
  `result_to_condition` helper now accepts only a `TenantSafeMessage` (or a type that converts
  into one through an explicit sanitizer), so a raw `kube::Error`, upstream API error, or
  secret-read error cannot be passed where a tenant-visible Condition message is produced
  without an explicit, reviewable sanitization step (#443).

## [1.2.3] - 2026-07-07

### Fixed

- Fix webhook rejecting valid Lidarr `appConfig` sections. The `validate_app_config_match` check was missing a `Lidarr` variant arm, so any Lidarr `ServarrApp` with `spec.appConfig.lidarr` set was always rejected at admission (#301).

## [1.2.2] - 2026-07-07

- Skip CRD publish when the CRD is unchanged.

## [1.2.1] - 2026-07-07

### Fixed

- Fix lidarr-youtube-downloader sidecar missing volume mounts (#293).

## [1.2.0] - 2026-07-07

### Added

- Add Plex sync to Maintainerr. Set `maintainerrSync.plexTokenSecret` to the name of a
  Secret containing a `plex-token` key (a plex.tv auth token); the operator discovers the
  in-cluster Plex `ServarrApp` and configures its hostname/port and auth token in
  Maintainerr automatically (#151).
- Add Lidarr YouTube Downloader sidecar support. Set `spec.appConfig.lidarr.youtubeDownloader`
  on a Lidarr `ServarrApp` to deploy the companion container alongside Lidarr. Supports
  `image`, `lidarrDbPath`, `lidarrMusicPath`, `ytCookiesFile`, `matchThreshold`, and
  `blacklistKeywords` configuration (#213).

### Changed

- Decouple CRD Helm chart (`servarr-crds`) version from the operator chart. The CRD
  chart version now only bumps when CRD files actually change (schema, validation rules,
  new fields). Bump `charts/servarr-crds/Chart.yaml`'s `version` field manually as
  needed; the app chart continues to bump on every release as before (#162).
- Update default Sonarr image to `4.0.18` (from `4.0.17`).
- Update default Jackett image to `0.24.2140` (from `0.24.2116`), rolling up upstream
  indexer-definition updates.
- Update default Subgen image to `2026.06.5` (from `2026.06.3`).

### Fixed

- Fix Maintainerr sync silently masking Kubernetes API errors (e.g. failing to read the
  Plex token secret). The operator now surfaces these errors as warnings instead of
  retrying silently (#265).
- Fix panic in resource builders (`pvc`, `networkpolicy`, `service`, `deployment`) when app defaults are missing for unknown app types. Builders now log the error and return a safe fallback instead of crashing (#267).
- Fix `maybe_run_backup` silently skipping backups when app defaults fail to load. Operator now logs a warning with the error context (#268).
- Fix default liveness probe (`timeout_seconds: 1`, `failure_threshold: 3`) being too
  aggressive for .NET-based *arr apps (Sonarr, Radarr, Lidarr). Brief HTTP unresponsiveness
  during RSS syncs, library scans, or GC pauses could trip 3 consecutive 1s-timeout failures
  in 30s and get the pod SIGKILLed even though it was healthy. Raised to `timeout_seconds: 5`,
  `failure_threshold: 5` (~50s grace). Override via `probes:` on the ServarrApp CR if you need
  different values (#173).
- Fix Maintainerr sync silently masking Sonarr/Radarr API list failures as "no servers
  registered", which caused duplicate server registrations on retry instead of a visible
  reconcile error. The operator now propagates these API errors so the controller retries
  with backoff instead of silently proceeding with a stale empty list (#199).
- Fix Maintainerr `DATA_DIR` not being wired to the config volume mount path. The operator
  now auto-injects `DATA_DIR` for Maintainerr equal to the `config` volume's `mountPath`
  (defaulting to `/opt/data`). Previously, users who mounted their PVC at `/config`
  (following the convention for other apps) would see a fresh empty database on every
  restart because Maintainerr reads from `DATA_DIR`, not `/config`. **Migration:** if your
  CR has `mountPath: /config` for the Maintainerr config volume, change it to
  `mountPath: /opt/data`; the files on the PVC do not need to move. You can also keep
  `mountPath: /config` and add `DATA_DIR: /config` to `spec.env` as an alternative.
- Fix Transmission ConfigMap silently producing empty settings when serialization fails (now logs a warning instead) (#269).

### Security

- Redact credential-bearing API error response bodies in Maintainerr sync logs. If
  Maintainerr ever echoes submitted credentials (API keys, plex.tv tokens) in a
  validation error message, those credentials are no longer logged verbatim in operator
  pod logs (#255).
- Add RFC 1123 label pattern and maxLength validation to `serviceName` CRD fields
  (`ServarrAppSpec`, `StackApp`, `Split4kOverrides`), preventing arbitrary strings
  from being forwarded as hostnames to downstream integrations (#256).

## [1.1.1] - 2026-07-01

### Changed

- Update default Sonarr image to `4.0.17` (from `4.0.16`).
- Update default Radarr image to `5.28.0` (from `5.27.0`).
- Update default Prowlarr image to `2.1.5.5019` (from `2.1.4.4941`).
- Update default Jackett image to `0.24.2116` (from `0.24.2050`).
- Update default Subgen image to `2026.06.3` (from `2026.06.1`).

## [1.1.0] - 2026-06-24

### Added

- Add `MediaStack` CRD for declarative multi-app deployment. Define a full media
  stack (Sonarr, Radarr, Lidarr, Prowlarr, Transmission, Jellyfin, etc.) as a single
  resource with stack-wide defaults (persistence, resources, networking) and
  per-app overrides.
- Add "Split 4K" support: `split4k: true` on a `StackApp` (Sonarr/Radarr only)
  automatically creates paired standard and 4K instances with independent root
  folders and quality profiles.
- Add automated backup/restore for Servarr v3 apps (Sonarr, Radarr, Lidarr,
  Prowlarr) via their native backup API, with configurable schedule and retention.
- Add SSH bastion app type for secure remote access to the media stack.

### Changed

- Restructure CRD versioning: `servarr-crds` chart now versions independently.
- Improve Prowlarr sync reliability with retry logic for transient API failures.

### Fixed

- Fix NFS volume mount ordering causing intermittent mount failures on pod restart.
- Fix GPU scheduling toleration not being applied to Deployment pod template.
- Fix backup schedule validation accepting invalid cron expressions.

## [1.0.3] - 2026-06-21

### Fixed

- Fix admission webhook rejecting valid `MediaStack` resources with `split4k`
  enabled due to a schema validation ordering bug.
- Fix Jellyfin startup wizard automation not completing library scan configuration
  on slow storage backends.
- Fix NetworkPolicy egress rules blocking DNS resolution in some CNI configurations.
- Fix resource requests/limits not being applied when only one of the two was
  specified in the CR spec.

## [1.0.2] - 2026-06-21

### Fixed

- Fix CRD chart publish workflow using the wrong version tag, causing a mismatch
  between the operator chart and CRD chart on install.

## [1.0.1] - 2026-06-18

### Changed

- Tighten default NetworkPolicy egress rules to explicitly allow only DNS,
  Kubernetes API server, and observability (OTLP) traffic.

### Fixed

- Fix Prowlarr indexer sync not retrying after a transient 5xx from a newly
  registered Sonarr/Radarr instance.

## [1.0.0] - 2026-06-18

### Added

- Initial release. Kubernetes operator for Sonarr, Radarr, Lidarr, Prowlarr,
  Transmission, SABnzbd, Jellyfin, Plex, Tautulli, Overseerr, Maintainerr, Bazarr,
  Subgen, and SSH bastion apps via the `ServarrApp` CRD.
- Add resource builders for Deployment, Service, PVC, NetworkPolicy, HTTPRoute,
  TCPRoute, and cert-manager Certificate.
- Add admission webhook for spec validation (app-type-specific config, duplicate
  instance detection, mount-path collisions).
- Add Prowlarr cross-app sync, Bazarr sync, Subgen-Jellyfin sync, Maintainerr sync.
- Add `servarrctl` CLI for local development: apply/delete/status for apps,
  backups, and managed apps/stacks, plus structured JSON logging.
- Add namespace-scoped and cluster-wide (`watchAllNamespaces`) operation, a
  Secret watcher for timely credential rotation, and a `crd` subcommand that
  prints CRD YAML.
- Add release automation: `cargo-release` + Keep a Changelog, with the
  multi-arch container image and Helm charts (CRDs + operator) published to GHCR
  on each `v*` tag.

<!-- next-url -->
[Unreleased]: https://github.com/phaedrus1992/servarr-operator/compare/v1.4.0...HEAD
[1.4.0]: https://github.com/phaedrus1992/servarr-operator/compare/v1.3.1...v1.4.0
[1.3.1]: https://github.com/phaedrus1992/servarr-operator/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/phaedrus1992/servarr-operator/compare/v1.2.3...v1.3.0
[1.2.3]: https://github.com/phaedrus1992/servarr-operator/compare/v1.2.2...v1.2.3
[1.2.2]: https://github.com/phaedrus1992/servarr-operator/compare/v1.2.1...v1.2.2
[1.2.1]: https://github.com/phaedrus1992/servarr-operator/compare/v1.2.0...v1.2.1
[1.2.0]: https://github.com/phaedrus1992/servarr-operator/compare/v1.1.1...v1.2.0
[1.1.1]: https://github.com/phaedrus1992/servarr-operator/compare/v1.1.0...v1.1.1
[1.1.0]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.3...v1.1.0
[1.0.3]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/phaedrus1992/servarr-operator/compare/50a4a1eb98121d552a37ba8dcf6f38043478d8d5...v1.0.0
