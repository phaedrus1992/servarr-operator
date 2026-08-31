# *arr Companion Apps: Unpackerr, Cleanuparr, Houndarr

**Date:** 2026-08-31
**Issues:** [#604](https://github.com/phaedrus1992/servarr-operator/issues/604) (Unpackerr),
[#605](https://github.com/phaedrus1992/servarr-operator/issues/605) (Cleanuparr),
[#606](https://github.com/phaedrus1992/servarr-operator/issues/606) (Houndarr)
**Milestone:** v1.4.0
**Depends on:** nothing
**Blocks:** nothing

---

## Problem

Three companion apps from the milestone thread, each investigated by the repo owner ahead of
this design pass (see the issue comments):

- **Unpackerr** — extracts archives for Sonarr/Radarr/Lidarr/Readarr after download. Config-file
  driven, no live API. Open question from #604: does it expose an HTTP port at all? Every
  `image-defaults.toml` entry has a `port`, and a "no probe" app would break that assumption.
- **Cleanuparr** — removes stalled/blocked/malicious downloads, triggers re-searches, manages
  seeding. Web-UI/DB configured; wiring the *arr connections needs a live REST call on reconcile.
- **Houndarr** — runs rate-limited missing/cutoff/upgrade searches against
  Radarr/Sonarr/Lidarr/Readarr/Whisparr. Same shape as Cleanuparr: web-UI configured, needs live
  REST wiring.

Per the owner's comment on #606, Cleanuparr and Houndarr are "the same kind of live-API
cross-app sync design" and were explicitly requested to be scoped together.

---

## Resolved: Unpackerr's port question

Confirmed against the canonical docs
([unpackerr.zip/docs/install/generated/webserver](https://unpackerr.zip/docs/install/generated/webserver/)):
Unpackerr has a `[webserver]` config block, default `listen_addr = "0.0.0.0:5656"`, but
`metrics = false` by default — the listener only serves content when metrics is turned on, and
it serves no UI, Prometheus metrics only.

Since the operator renders Unpackerr's config file itself (see below), it can simply always set
`webserver.metrics = true` in the generated config and probe port `5656`. No new "no probe"
`ProbeType` variant is needed — every `image-defaults.toml` entry keeps its `port` field.

---

## Resolved: Cleanuparr and Houndarr's actual API surface

Verified against each project's own source (not just docs — Cleanuparr's public docs only cover
its read-only `/api/stats`, and Houndarr publishes no API reference for instance config at all):

- **Cleanuparr** has a full JSON REST contract:
  `code/backend/Cleanuparr.Api/Features/Arr/Controllers/ArrConfigController.cs` exposes
  `GET /api/configuration/{sonarr,radarr,lidarr,readarr,whisparr}`,
  `POST .../instances` (create), `PUT .../instances/{id}` (update),
  `DELETE .../instances/{id}` (delete), all `[Authorize]`-gated behind Cleanuparr's own API key
  (`X-Api-Key` header or JWT bearer). This is a drop-in fit for the `CrossAppSync` trait, same
  shape as Maintainerr.
- **Houndarr** has no such contract. Its only `/api/*` routes
  (`src/houndarr/routes/api/{widget,logs,status}.py`) are a read-only dashboard widget (whose
  key explicitly does not authorize settings or writes), logs, status, and a run-now trigger.
  Every instance-CRUD route lives under `/settings/instances/*`
  (`src/houndarr/routes/settings/instances.py`) as FastAPI `Form()` endpoints returning HTML
  partials for HTMX — session-cookie authenticated, with a double-submit CSRF cookie
  (`src/houndarr/auth/csrf.py`), not JSON.

Per direction from this design session: build the session/form automation for Houndarr now
rather than drop it from scope, and file a follow-up issue to pursue a proper JSON API upstream
in the Houndarr project (potentially contributing it) so the scraper can be retired later.

---

## Architecture

Three new `AppType` variants — `Unpackerr`, `Cleanuparr`, `Houndarr` — added to the enum in
`crates/servarr-crds/src/v1alpha1/spec.rs:139-157`, alongside the existing 13.

Unpackerr follows the existing Bazarr-subgen init-container bootstrap pattern (see
`docs/superpowers/plans/2026-04-13-bazarr-subgen.md`): the operator renders
`/config/unpackerr.conf` from CRD spec fields via an init container, no live API involved.

Cleanuparr and Houndarr both implement the same `CrossAppSync` trait extracted from Maintainerr's
existing list-then-add pattern (`sync_maintainerr_servers`,
`crates/servarr-operator/src/controller.rs:3964`) — but `HoundarrClient`'s implementation is
form/session automation rather than a JSON HTTP call, per the resolved API-surface finding above.
The trait boundary is what keeps `sync_cross_app` (below) identical for all three; only the
client internals differ per app.

**Scope note on the extraction:** `sync_maintainerr_servers` does more than *arr registration —
it also sets Seerr, Tautulli, and Plex as singleton config values (`set_seerr`, `set_tautulli`,
`set_plex_token`, `set_plex` in `maintainerr.rs`). Those calls are Maintainerr-specific and stay
bespoke. The trait covers only the part all three apps share: list the currently-registered
Sonarr/Radarr(/Lidarr/Readarr) instances, then register any discovered instance that's missing.

---

## API surface — `crates/servarr-api`

### `CrossAppSync` trait

```rust
/// One *arr instance as a companion app models it in its own registration API.
pub struct RegisteredArrInstance { pub name: String, pub base_url: String, pub api_key: String }

#[async_trait]
pub trait CrossAppSync {
    /// List instances of one *arr kind already registered in the companion app.
    async fn list_registered(&self, kind: AppType) -> Result<Vec<String>, ApiError>;

    /// Register one *arr instance. Callers only call this for a name absent from
    /// `list_registered` — the trait itself does not de-duplicate.
    async fn register(&self, kind: AppType, instance: &RegisteredArrInstance) -> Result<(), ApiError>;
}
```

`MaintainerrClient` (`maintainerr.rs`) implements `CrossAppSync` for its existing
`list_sonarr`/`add_sonarr`/`list_radarr`/`add_radarr` pairs, unchanged in behavior — its
Seerr/Tautulli/Plex methods stay outside the trait.

`cleanuparr.rs` implements `CrossAppSync` as a plain JSON `HttpClient`, matching
`MaintainerrClient`'s shape: `list_registered` calls `GET /api/configuration/{kind}` and reads
`.instances[].name`; `register` calls `POST /api/configuration/{kind}/instances` with
`{name, type, url, apiKey}` (`ArrInstanceRequest`'s fields per `ArrConfigController.cs`),
authenticated with Cleanuparr's own API key via `X-Api-Key`.

`houndarr.rs` implements the same trait via session automation, not JSON: on construction, GET
`/login` to obtain the CSRF cookie, POST `/login` with the operator-held admin
username/password (`Form` fields `username`/`password`) to establish a session cookie,
persisting both cookies (`reqwest::cookie::Jar`) across calls. `list_registered` GETs
`/settings/instances` (or parses the dashboard partial — exact scrape target confirmed at
implementation time) and extracts registered instance names from the rendered HTML.
`register` POSTs `/settings/instances` with the same CSRF cookie's token plus
`name`/`type`/`url`/`api_key` form fields, and treats a 200/303 response as success (per
`instance_create`'s handler in `settings/instances.py`). This is materially more fragile than
the other two clients — it breaks on any upstream template/route change — and is scoped as a
stopgap pending the upstream JSON API follow-up issue.

### Generic sync function

`crates/servarr-operator/src/controller.rs` gains:

```rust
async fn sync_cross_app<T: CrossAppSync>(
    client: &T,
    app_name: &str,
    discovered: &[DiscoveredApp],
) -> Result<SyncCounts, TenantSafeMessage>
```

replacing the Sonarr/Radarr list-then-add block inside `sync_maintainerr_servers` with a call to
this function, and adding two call sites — one for Cleanuparr, one for Houndarr — each gated on
their own `*_sync.enabled` flag, mirroring the existing dispatch block at
`controller.rs:535-646`.

---

## CRD — `CleanuparrSyncSpec`, `HoundarrSyncSpec`

Mirrors `MaintainerrSyncSpec` (`crates/servarr-crds/src/v1alpha1/types.rs:750-761`) minus the
Plex-specific field:

```rust
pub struct CleanuparrSyncSpec {
    pub enabled: bool,                    // default false
    pub namespace_scope: Option<String>,  // default: the CR's own namespace
}

pub struct HoundarrSyncSpec {
    pub enabled: bool,
    pub namespace_scope: Option<String>,
    /// Secret holding Houndarr's own admin login (keys: `username`, `password`), needed
    /// for the session-cookie login the scraper client performs. Same Secret shape as the
    /// existing admin-credentials pattern (`docs/admin-credentials.md`).
    pub admin_credentials_secret: String,
}
```

Houndarr's spec field is required, not optional like the others' `namespace_scope` — the
scraper client cannot function without a login, so `houndarrSync.enabled: true` without
`adminCredentialsSecret` set is a validation error at the CRD or reconcile level (spec's choice
at implementation time).

Both fields go on `ServarrAppSpec` next to the other `*_sync` fields
(`crates/servarr-crds/src/v1alpha1/spec.rs`, near line 126).

### `image-defaults.toml`

Three new blocks:

```toml
[unpackerr]
# image: golift/unpackerr (root) or ghcr.io/unpackerr/unpackerr (PUID/PGID) — pick at
# implementation time; port 5656 (webserver.metrics, forced on by the rendered config)

[cleanuparr]
# image: ghcr.io/cleanuparr/cleanuparr:latest; port 11011

[houndarr]
# image: ghcr.io/av1155/houndarr:latest; port 8877
```

---

## Status conditions

One condition per app type, following `MaintainerrSyncReady`
(`crates/servarr-crds/src/v1alpha1/status.rs`): `CleanuparrSyncReady`, `HoundarrSyncReady`. Sync
failure doesn't block the rest of reconcile — same per-app independent-failure handling as
`sync_maintainerr_servers` (one unreachable *arr logs a warning and doesn't abort the sweep).

Unpackerr gets no sync condition — it has no live API — but gets the same init-container
condition as Bazarr-subgen's config bootstrap.

---

## Tests

- **`CrossAppSync` trait** — a shared test helper (wiremock-backed) exercised against all three
  implementations, asserting `list_registered` + `register` shape.
- **`sync_cross_app`** — unit tests for the three counted outcomes (absent → registered, present →
  skipped, list failure → aborts before any write), decoupled from any specific client.
- **Cleanuparr client tests** — `cleanuparr.rs`, wiremock-backed, mirroring `maintainerr.rs`'s
  existing proptest + unit test shape.
- **Houndarr client tests** — `houndarr.rs`, wiremock-backed against fixture HTML fixtures for
  `/login` and the instances form/response, covering: CSRF cookie extraction, session-cookie
  persistence across calls, login failure (bad credentials → clear `ApiError`, not a panic),
  and HTML-scrape parsing of the registered-instances list.
- **Unpackerr config renderer** — golden-file test asserting the rendered
  `/config/unpackerr.conf` always includes `webserver.metrics = true`, matching the Bazarr-subgen
  renderer's existing test pattern.
- **CRD tests** — `CleanuparrSyncSpec`/`HoundarrSyncSpec` defaults, schema regeneration, and that
  the 3 new `AppType` variants round-trip through serde.
- **Controller reconcile tests** — one per app type, mirroring
  `sync_maintainerr_servers_sanitizes_list_sonarr_response_body` (`controller.rs:7913`).

Before claiming done: `cargo clippy --all-targets --all-features -- -D warnings` (CI runs a
newer toolchain than most local setups), `cargo fmt`, `cargo test`, and CRD regeneration with no
schema drift.

---

## Acceptance criteria

- [ ] `AppType` gains `Unpackerr`, `Cleanuparr`, `Houndarr`; `image-defaults.toml` gains matching
      entries with real image references confirmed at implementation time.
- [ ] Unpackerr: init-container renders `/config/unpackerr.conf` from CRD spec fields, always
      setting `webserver.metrics = true`; port `5656` probed for readiness.
- [ ] `CrossAppSync` trait extracted from Maintainerr's existing Sonarr/Radarr list-then-add
      logic; `MaintainerrClient` implements it without behavior change.
- [ ] `CleanuparrClient` implements `CrossAppSync` against `ArrConfigController`'s JSON API
      (`/api/configuration/{kind}/instances`), API-key authenticated.
- [ ] `HoundarrClient` implements `CrossAppSync` via session-cookie login + CSRF-tokened form
      posts against `/settings/instances`, using the `adminCredentialsSecret`-sourced login.
- [ ] `cleanuparrSync` / `houndarrSync` on the respective CR registers discovered *arr instances
      idempotently across reconciles, with per-app independent failure handling.
- [ ] Follow-up issue filed: investigate/pursue a proper JSON API for Houndarr instance config
      upstream, so `HoundarrClient`'s scraper can be retired later.
- [ ] `CleanuparrSyncReady` / `HoundarrSyncReady` status conditions, matching the
      `MaintainerrSyncReady` pattern.
- [ ] Tests per the Tests section above, including the golden-file Unpackerr config test.
- [ ] CRDs regenerated; `docs/configuration.md` covers all three apps and their sync behavior.
- [ ] `CHANGELOG.md` entry under `Added`.
