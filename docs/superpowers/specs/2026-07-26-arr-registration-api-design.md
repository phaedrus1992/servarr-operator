# Download Client and Indexer Registration API Design

**Date:** 2026-07-26
**Issue:** [#363](https://github.com/phaedrus1992/servarr-operator/issues/363) — download-client + indexer auto-wiring into Sonarr/Radarr/Lidarr
**Epic:** [#366](https://github.com/phaedrus1992/servarr-operator/issues/366) · index: [`2026-07-26-downloader-epic-index.md`](2026-07-26-downloader-epic-index.md)
**Milestone:** v2.0.0
**Depends on:** nothing
**Blocks:** [#365](https://github.com/phaedrus1992/servarr-operator/issues/365)

---

## Problem

`ServarrClient` exposes `system_status`, `health`, `root_folder`, `updates`, backup CRUD,
and `configure_admin`, and nothing else. There is no `/api/v3/downloadclient` or
`/api/v3/indexer` call anywhere in the repo. The operator deploys SABnzbd and Transmission
alongside Sonarr/Radarr/Lidarr and never connects them, so a user who deploys a complete
stack still has to open each \*arr and add the download client by hand.

This spec adds that registration surface and uses it to wire up the existing download
clients. The downloader companion service (#365) then reuses the same surface to register
itself.

---

## Deviation from the issue: direct HTTP, not the SDK

#363 proposes going through the devopsarr SDK's `download_client_api` and `indexer_api`.
Don't. The existing SDK-backed methods each need a four-arm `match` on `AppKind` plus a
per-app conversion macro, because every SDK crate declares its own structurally identical
model type. For download clients and indexers that cost buys nothing: the payload is
`{id, name, implementation, configContract, fields: [{name, value}], ...}`, and the
`fields` array is untyped in the SDK exactly as it is on the wire. Four dispatch arms and
four conversion macros would wrap a JSON blob in order to produce the same JSON blob.

Use `HttpClient` with one shared serde type instead. The precedent is `create_backup`
(`servarr_v3.rs:433`), and the endpoint path is identical across Sonarr, Radarr, and
Lidarr. This is roughly a quarter of the code and the same behaviour.

One caveat: `ServarrClient`'s internal `HttpClient` is built against `/api/v3/`
(`servarr_v3.rs:277`), which is correct for Sonarr/Radarr/Lidarr but wrong for Prowlarr,
which serves `/api/v1/`. Prowlarr indexer registration is a #365 concern and belongs in
`prowlarr.rs`, which already has its own client. `ServarrClient`'s new methods return
`ApiError` for `AppKind::Prowlarr` rather than silently hitting a 404 path.

---

## API surface — `crates/servarr-api/src/servarr_v3.rs`

### Types

```rust
/// One entry in an *arr download-client or indexer `fields` array.
pub struct ConfigField { pub name: String, pub value: serde_json::Value }

/// A download client as the *arr apps model it.
pub struct DownloadClientSpec {
    pub name: String,
    pub implementation: String,   // "Sabnzbd" | "Transmission"
    pub config_contract: String,  // "SabnzbdSettings" | "TransmissionSettings"
    pub enable: bool,
    pub priority: i32,
    pub fields: Vec<ConfigField>,
}

pub struct IndexerSpec {
    pub name: String,
    pub implementation: String,   // "Newznab"
    pub config_contract: String,  // "NewznabSettings"
    pub protocol: String,         // "usenet"
    pub enable_rss: bool,
    pub enable_automatic_search: bool,
    pub enable_interactive_search: bool,
    pub priority: i32,
    pub fields: Vec<ConfigField>,
}

/// What a reconcile call actually did — lets the controller log and set conditions
/// without re-querying.
pub enum Reconciled { Created(i64), Updated(i64), Unchanged(i64) }
```

Both spec types get constructor helpers so callers do not hand-assemble `fields`:
`DownloadClientSpec::sabnzbd(name, host, port, api_key, category, use_ssl, url_base)` and
`DownloadClientSpec::transmission(name, host, port, username, password, category,
directory)`, plus `IndexerSpec::newznab(name, base_url, api_path, api_key, categories)`.
Keeping field-name strings inside these constructors means a typo is a compile-once
mistake in one place rather than a runtime 400 from four call sites.

### Methods

```rust
list_download_clients()             -> Result<Vec<DownloadClientEntry>, ApiError>
add_download_client(&spec)          -> Result<i64, ApiError>
update_download_client(id, &spec)   -> Result<(), ApiError>
delete_download_client(id)          -> Result<(), ApiError>
reconcile_download_client(&spec)    -> Result<Reconciled, ApiError>
```

and the same five for indexers. `reconcile_*` is the method callers actually use: it
lists, matches on `name`, and creates or updates accordingly. `Unchanged` is returned when
the existing entry already deep-equals the desired one, which keeps reconcile logs quiet
across the steady state.

Comparison for `Unchanged` is on the fields the operator sets, not on the whole resource.
The \*arr apps populate defaults and computed properties the operator never sends, so a
whole-resource equality check would report a difference on every single reconcile and
issue a pointless PUT forever.

---

## CRD — `DownloadClientSyncSpec`

Follows `ProwlarrSyncSpec` (`types.rs:613`) exactly, plus category control:

```rust
pub struct DownloadClientSyncSpec {
    pub enabled: bool,                          // default false
    pub namespace_scope: Option<String>,        // default: the CR's own namespace
    pub auto_remove: bool,                      // default true
    pub categories: Option<CategoryOverrides>,  // per-*arr category names
    pub target_apps: Option<Vec<String>>,       // allowlist of *arr CR names
}

pub struct CategoryOverrides { pub sonarr: Option<String>, pub radarr: Option<String>, pub lidarr: Option<String> }
```

The field goes on `ServarrAppSpec` next to the other `*_sync` fields (`spec.rs:108-126`).
It lives on the **download client's** CR and registers that client into the discovered
\*arrs — the mirror image of `prowlarr_sync`, which lives on Prowlarr and registers the
\*arrs into it.

Default categories are `tv` for Sonarr, `movies` for Radarr, and `music` for Lidarr.

---

## Controller — `sync_download_client_arrs`

Modelled on `sync_prowlarr_apps` (`controller.rs:2045`) and `sync_bazarr_apps`
(`controller.rs:2528`), gated in the `reconcile()` `*_sync` dispatch block
(`controller.rs:535-646`) and reporting a `download_client_sync` condition alongside the
existing ones (`controller.rs:1297-1305`).

Flow:

1. Return early unless `spec.app` is a download client type and `download_client_sync.enabled`.
2. Read the client's own host, port, and API key. SABnzbd's key comes from its managed
   Secret the same way `sabnzbd.rs` callers already read it; Transmission's credentials
   come from its settings.
3. `discover_namespace_apps` (`controller.rs:1981`) for Sonarr/Radarr/Lidarr in scope,
   filtered by `target_apps` when set, reading each app's API-key Secret.
4. For each: build the `DownloadClientSpec` named `servarr-operator/<cr-name>` with the
   per-app category, and call `reconcile_download_client`.
5. When `auto_remove`, list clients, and delete any whose name starts with
   `servarr-operator/` and whose CR no longer exists in scope.

Per the operator reconciliation rules, each \*arr is handled independently: one
unreachable app logs a warning and does not abort the rest of the sweep. The overall sync
condition is `False` if any target failed, with the failing app names in the message.

Registering a category on the client entry is enough; the operator does not need to create
the category inside SABnzbd. SABnzbd accepts an unknown category on an add and files it
under the default, and the downloader service (#364) reports whichever category it was
handed. Creating categories is extra API surface for no behavioural gain.

---

## Path mapping caveat

Document, do not build. See the shared contract in the epic index: the `storage` path a
download client reports must resolve identically inside the \*arr container. The operator
mounts the shared downloads PVC at the same `mountPath` everywhere, so the default stack
works. Add a warning to the `docs/configuration.md` download-client sync section stating
that a divergent `mountPath` breaks imports, and that remote path mapping is not
implemented.

---

## Tests

- **wiremock, `crates/servarr-api/tests/api_tests.rs`** — list/add/update/delete against a
  mocked \*arr for each verb; `reconcile_download_client` covering the three outcomes
  (absent → `Created`, present-and-different → `Updated`, present-and-equal →
  `Unchanged`); `AppKind::Prowlarr` returns an error rather than calling `/api/v3/`.
- **Constructor tests** — `DownloadClientSpec::sabnzbd` emits exactly the field names
  SABnzbd's config contract requires; same for Transmission and Newznab. These are the
  round-trip guard against silent 400s.
- **CRD tests** — `DownloadClientSyncSpec` defaults (`enabled` false, `auto_remove` true),
  schema regeneration, and that omitting the field leaves existing CRs valid.
- **Controller reconcile test** — a SABnzbd CR with `downloadClientSync.enabled` produces
  a download-client registration call against a mocked Sonarr with category `tv`; a second
  reconcile produces no write; `auto_remove` deletes a `servarr-operator/`-prefixed entry
  whose CR is gone and leaves a hand-added entry alone.

Before claiming done: `cargo clippy --all-targets --all-features -- -D warnings` (CI runs
1.94.0, which is stricter than most local toolchains), `cargo fmt`, `cargo test`, and CRD
regeneration with no schema drift.

---

## Acceptance criteria

Mirrors #363, adjusted for the SDK deviation.

- [ ] `servarr_v3.rs` gains list/add/update/delete/reconcile for both download clients and
      indexers, reconcile-by-name, via `HttpClient` with typed spec constructors.
- [ ] `downloadClientSync` on a SABnzbd or Transmission CR registers it in every discovered
      Sonarr/Radarr/Lidarr with the correct default category, idempotently across reconciles.
- [ ] `auto_remove` cleans up stale `servarr-operator/`-prefixed entries and never touches
      hand-added ones.
- [ ] wiremock tests for the new API methods; a reconcile test proving a SABnzbd CR results
      in a registration call against a mocked \*arr.
- [ ] CRDs regenerated; `docs/configuration.md` covers the sync and the identical-path
      requirement.
- [ ] `CHANGELOG.md` entry under `Added`.
