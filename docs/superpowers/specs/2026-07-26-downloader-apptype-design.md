# Downloader AppType and Auto-Wiring Design

**Date:** 2026-07-26
**Issue:** [#365](https://github.com/phaedrus1992/servarr-operator/issues/365) — add Downloader AppType + auto-wire it into the \*arr apps
**Epic:** [#366](https://github.com/phaedrus1992/servarr-operator/issues/366) · index: [`2026-07-26-downloader-epic-index.md`](2026-07-26-downloader-epic-index.md)
**Milestone:** v2.0.0
**Depends on:** [#363](https://github.com/phaedrus1992/servarr-operator/issues/363) (registration API), [#364](https://github.com/phaedrus1992/servarr-operator/issues/364) (the image)
**Blocks:** [#379](https://github.com/phaedrus1992/servarr-operator/issues/379)

---

## Scope

Deploy the `servarr-downloader` service as a first-class app type and wire it into every
Sonarr/Radarr/Lidarr in the namespace as both a SABnzbd download client and a Newznab
indexer, so downloading from YouTube and Apple Music needs no manual \*arr configuration.
This is the standard "add an app" surface plus one cross-app sync plus one thing no other
app in the repo needs: pushing the \*arr credentials back *into* the deployed service.

---

## App definition

### `image-defaults.toml`

```toml
[downloader]
repository = "ghcr.io/phaedrus1992/servarr-downloader"
tag = "0.1.0"   # first tag published by #364; bump via the normal image sweep
port = 8080
security = "nonroot"
downloads = true
probe_path = "/health"
```

Then `scripts/sync-image-defaults.sh` to regenerate the chart values block.

### AppType

**Correction to the issue:** `AppType` lives in `crates/servarr-crds/src/v1alpha1/spec.rs`
(`spec.rs:185` for `tier()`), not `types.rs` as #365 states. `types.rs` is where the sync
specs live. Add `Downloader` with `as_str()` → `"downloader"` and `tier()` → 1, alongside
Sabnzbd and Transmission, and update the tier doc comment at `spec.rs:180-184`.

Tier 1 means the Downloader starts before the \*arr apps in tier 2. That is correct — it is
a download client — but it inverts the dependency for metadata reflect-back, which needs
the \*arr apps. Nothing breaks: reflect-back happens at search time, not at startup, and
#364's `/health` deliberately does not depend on the \*arr apps. The sync that wires them
together runs on reconcile and retries until they are up.

### Pod composition

Two containers, which is what makes this app unlike the others:

1. **`downloader`** — the service. `/config` PVC for job state and config, shared downloads
   volume at the same `mountPath` as the \*arr apps, `DOWNLOADER_API_KEY` from the
   operator-managed Secret, optional `TZ`.
2. **`pot-provider`** — `brainicism/bgutil-ytdlp-pot-provider`, reached over localhost.

The sidecar is not optional decoration. `yt-dlp` from a cluster IP reliably hits "Sign in
to confirm you're not a bot", and YouTube is enforcing GVS PO tokens across most player
clients, so cookies alone no longer suffice. Without the provider the feature's headline
use case fails unattended. Pin its image in `image-defaults.toml` as a second entry so it
is visible and updatable like every other image the operator ships.

An optional cookies Secret mounts at `/config/cookies.txt` for age-restricted or
members-only content, layered on top rather than instead.

### API key

Reuse `ensure_api_key_secret` (`controller.rs:729`) with a `Downloader` arm following
Bazarr's deterministic-secret handling. One key covers the Newznab surface, the SABnzbd
surface, and the admin API.

---

## `AppConfig::Downloader(DownloaderConfig)`

```rust
pub struct DownloaderConfig {
    pub indexer_via: Option<IndexerVia>,        // Direct | Prowlarr | Both, default Direct
    pub min_confidence: Option<f64>,            // default 0.75
    pub categories: Option<CategoryOverrides>,  // reuse #363's type
    pub cookies_secret: Option<String>,
    pub acoustid_key_secret: Option<String>,
    pub quality_map: Option<serde_json::Value>, // inline overrides written to /config
}
```

Wire the `(AppType::Downloader, AppConfig::Downloader(_))` arm into
`validate_app_config_match()` (`webhook.rs:361`).

---

## Cross-app sync — `downloaderSync`

```rust
pub struct DownloaderSyncSpec {
    pub enabled: bool,                    // default false
    pub namespace_scope: Option<String>,
    pub auto_remove: bool,                // default true
    pub indexer_via: IndexerVia,          // default Direct
    pub target_apps: Option<Vec<String>>,
}
```

`sync_downloader_arrs` follows `sync_prowlarr_apps` (`controller.rs:2045`), gated in the
`reconcile()` dispatch block (`controller.rs:535-646`) with its own status condition.

Three things happen per reconcile, each independently checked so a partial failure retries
cleanly rather than blocking the rest:

### 1. Push the `arrs` table into the service

This step is unique to the Downloader and has no precedent in the repo. Discover the
\*arr apps with `discover_namespace_apps` (`controller.rs:1981`), read each one's API-key
Secret, and `PUT` the resulting table to the service's admin API:

```json
{"arrs": [{"slug": "sonarr-main", "kind": "sonarr",
           "base_url": "http://sonarr.media.svc:8989", "api_key": "…"}]}
```

Slug is the \*arr's CR name. Using the live admin API rather than a ConfigMap avoids a pod
restart whenever an \*arr is added or removed, and matches how the operator already syncs
Prowlarr, Bazarr, and Overseerr — live API calls on every reconcile.

This step must come first. Registering an indexer URL for a slug the service does not yet
know about produces a 404 on the \*arr's connection test.

### 2. Register the download client

Always, via #363's `reconcile_download_client`. `implementation = "Sabnzbd"`, host
`<svc>.<ns>.svc`, the service port, the operator API key, `urlBase = /api/sabnzbd`, and the
per-app category. Named `servarr-operator/<cr-name>` per the shared naming contract.

### 3. Register the indexer

Per-\*arr URL so the service can identify the caller for metadata reflect-back:
`http://<svc>.<ns>.svc:8080/arr/<slug>`, API path `/api`.

| `indexerVia` | Behaviour |
|--------------|-----------|
| `direct` (default) | `reconcile_indexer` straight into each Sonarr/Radarr/Lidarr. No Prowlarr needed. |
| `prowlarr` | Register once in Prowlarr as a Generic Newznab indexer and let Prowlarr's existing app-sync push it down. Requires Prowlarr deployed. |
| `both` | Both, de-duplicated by name so no \*arr receives the indexer twice. |

`prowlarr` and `both` need an indexer method on `prowlarr.rs`, which serves `/api/v1/` —
#363's `ServarrClient` methods are `/api/v3/` and explicitly error for `AppKind::Prowlarr`.

Note the per-\*arr URL and Prowlarr are in tension: a single Prowlarr-registered indexer
has one URL and therefore one slug, so every \*arr behind it identifies as the same app.
For `prowlarr` mode, register the Prowlarr instance itself as the slug and resolve
metadata through Prowlarr's own \*arr application list. If that proves unreliable in
practice, `direct` remains the default for exactly this reason, and `prowlarr` mode should
be documented as best-effort.

`auto_remove` sweeps stale `servarr-operator/`-prefixed download clients and indexers whose
CRs are gone.

---

## Operator touchpoints

- `image-defaults.toml` — `[downloader]` and the pot-provider pin, plus `sync-image-defaults.sh`
- `crates/servarr-crds/src/v1alpha1/spec.rs` — `AppType::Downloader`, `as_str()`, `tier()`, tier doc comment, `downloader_sync` field
- `crates/servarr-crds/src/v1alpha1/types.rs` — `DownloaderSyncSpec`, `IndexerVia`
- `crates/servarr-crds/src/v1alpha1/defaults.rs` — `validate_all()` `all` array; default spec with config and downloads PVCs, nonroot
- `crates/servarr-crds/src/v1alpha1/app_config.rs` — `AppConfig::Downloader`
- `crates/servarr-operator/src/webhook.rs` — `validate_app_config_match()` arm
- `crates/servarr-operator/src/context.rs` — `load_image_overrides()` apps array
- `crates/servarr-resources/src/deployment.rs` — API-key env, pot-provider sidecar, optional cookies mount
- `crates/servarr-operator/src/controller.rs` — `ensure_api_key_secret` arm, PVCs, `sync_downloader_arrs`, dispatch and condition
- `charts/servarr-crds/templates/*` — regenerated CRDs
- `docs/` — image table, config section, `docs/examples/downloader.yaml`, `.github/smoke-test/manifests/downloader.yaml`

---

## Tests

- **CRD** — `defaults_tests` and `crd_tests` for the new AppType, defaults, and
  `DownloaderSyncSpec`; `indexerVia` enum round-trips through the schema.
- **Deployment builder** — pod has both containers; downloads volume mount path matches
  what the \*arr apps get; cookies Secret mounts only when configured.
- **Webhook** — `AppType::Downloader` with a non-Downloader `AppConfig` is rejected.
- **Reconcile** — against mocked \*arrs, assert the ordering (`arrs` push precedes indexer
  registration), that both a download-client and an indexer registration fire, and one case
  per `indexerVia` variant including `both` producing no duplicate. Assert idempotence on a
  second reconcile and that `auto_remove` clears a stale entry.
- **Smoke test** — a manifest that deploys the Downloader alongside a Sonarr and asserts
  the registration lands.

---

## Acceptance criteria

- [ ] `Downloader` deploys: nonroot, port 8080, `/config` PVC, shared downloads volume,
      `DOWNLOADER_API_KEY` from an operator-managed Secret.
- [ ] The pod includes the bgutil PO-token provider sidecar, pinned in `image-defaults.toml`.
- [ ] `downloaderSync` pushes the `arrs` table to the service before registering anything.
- [ ] It registers the service as a SABnzbd download client in every discovered
      Sonarr/Radarr/Lidarr with the right per-app category.
- [ ] `indexerVia: direct` registers a Generic Newznab indexer in each \*arr at its
      per-\*arr URL; `prowlarr` registers via Prowlarr; `both` does both without duplicates.
- [ ] `auto_remove` cleans up stale registrations; sync is idempotent across reconciles.
- [ ] CRDs regenerated; unit and reconcile tests; smoke-test manifest;
      `docs/examples/downloader.yaml`.
- [ ] Docs cover the identical-downloads-path requirement, the `indexerVia` choice and its
      Prowlarr caveat, why the PO-token sidecar exists, and that the `gamdl` backend (#379)
      needs Apple Music credentials before it can grab.
