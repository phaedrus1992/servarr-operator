# Navidrome + Poutine App Support

**Date:** 2026-04-29  
**Status:** Approved

## Overview

Add two new `AppType` variants — `Navidrome` and `Poutine` — following the existing pattern
for registering managed apps. Navidrome is a self-hosted music server (tier 0, media server).
Poutine is a federated music library hub that wraps Navidrome (tier 3, ancillary).

## Image Defaults

### Navidrome

- **Image:** `deluan/navidrome:0.61.2`
- **Port:** 4533
- **Security profile:** `nonroot`
- **Probe:** HTTP `/`
- **Downloads PVC:** false
- **Default PVC:** `data` → `/data` (1Gi, SQLite DB + thumbnails)
- **Music library:** user-supplied NFS mount (no operator default)

### Poutine

- **Image:** `ghcr.io/benders/poutine:0.4.5`
- **Port:** 3000
- **Security profile:** `nonroot`
- **Probe:** HTTP `/`
- **Downloads PVC:** false
- **Default PVC:** `data` → `/app/data` (1Gi, SQLite DB + ed25519 private key + cover-art cache)

The `/app/config` directory (peers.yaml) is **not** a PVC. It is mounted from an
operator-generated ConfigMap when `appConfig.poutine.peers` is set, or omitted entirely
when no peers are configured. This matches how Prowlarr handles custom indexer definitions.

## Tiers

| App        | Tier | Tier name     | Reason                              |
|------------|------|---------------|-------------------------------------|
| Navidrome  | 0    | MediaServers  | Music server, no upstream deps      |
| Poutine    | 3    | Ancillary     | Depends on Navidrome (tier 0)       |

## AppConfig: PoutineConfig

Add `Poutine(PoutineConfig)` to the `AppConfig` enum in `app_config.rs`.

```rust
pub struct PoutineConfig {
    pub peers: Vec<PoutinePeer>,
}

pub struct PoutinePeer {
    pub id: String,         // peer's POUTINE_INSTANCE_ID
    pub url: String,        // base URL of the peer hub (e.g. "https://music.example.com")
    pub public_key: String, // ed25519 public key string (e.g. "ed25519:fooBARbaz==")
}
```

When `peers` is non-empty, a ConfigMap is generated containing `peers.yaml` (YAML list of
peer objects) and mounted read-only at `/app/config/peers.yaml` inside the Poutine container.
When `peers` is empty or `appConfig` is absent, no ConfigMap is created and the mount is
omitted — Poutine runs in standalone mode.

### Generated peers.yaml shape

```yaml
- id: "friend-instance"
  url: "https://music.friend.example.com"
  public_key: "ed25519:fooBARbaz=="
```

## Default Env Vars

Injected as defaults in `AppDefaults::for_app` (alongside `TZ=UTC`).

### Navidrome

| Var                          | Value  |
|------------------------------|--------|
| `ND_LOGLEVEL`                | `info` |
| `ND_SCANSCHEDULE`            | `1h`   |
| `ND_SESSIONTIMEOUT`          | `24h`  |
| `ND_ENABLEEXTERNALSERVICES`  | `false`|

### Poutine

| Var                        | Value                              |
|----------------------------|------------------------------------|
| `NODE_ENV`                 | `production`                       |
| `DATABASE_PATH`            | `/app/data/poutine.db`             |
| `POUTINE_PRIVATE_KEY_PATH` | `/app/data/poutine_ed25519.pem`    |
| `POUTINE_PEERS_CONFIG`     | `/app/config/peers.yaml`           |

`NAVIDROME_URL`, `NAVIDROME_USERNAME`, `NAVIDROME_PASSWORD`, `POUTINE_INSTANCE_ID`,
`POUTINE_OWNER_USERNAME`, and `POUTINE_OWNER_PASSWORD` are left to the user — they contain
credentials or instance-specific values that belong in a Secret or the CR's env override,
not compiled-in defaults.

`POUTINE_PEERS_CONFIG` is only injected when `appConfig.poutine.peers` is non-empty (i.e.
when the ConfigMap and mount are actually created). When peers is empty, the env var is
omitted so Poutine does not attempt to read a non-existent config file.

## Files to Change

### `image-defaults.toml`

Add `[navidrome]` and `[poutine]` sections.

### `crates/servarr-crds/src/v1alpha1/spec.rs`

- Add `Navidrome` and `Poutine` variants to `AppType` enum.
- Add arms to `as_str()`: `"navidrome"`, `"poutine"`.
- Add arms to `tier()`: Navidrome → 0, Poutine → 3.

### `crates/servarr-crds/src/v1alpha1/defaults.rs`

- Add both variants to the exhaustive `validate_all()` array.
- In `for_app()`, add special cases:
  - **Navidrome:** push default env vars (ND_*).
  - **Poutine:** push default env vars (NODE_ENV, DATABASE_PATH, etc.); override default
    PVC name/path from `config`/`/config` → `data`/`/app/data`.

### `crates/servarr-crds/src/v1alpha1/app_config.rs`

- Add `PoutineConfig` and `PoutinePeer` structs.
- Add `Poutine(PoutineConfig)` variant to `AppConfig`.

### `crates/servarr-resources/src/configmap.rs`

- Add `build_poutine_peers(app) -> Option<ConfigMap>` that serializes
  `PoutineConfig::peers` to YAML and returns a ConfigMap with key `peers.yaml`.
  Returns `None` when peers list is empty.
- Wire into `build()`: `AppType::Poutine => build_poutine_peers(app)`.

### `crates/servarr-resources/src/deployment.rs`

- When building a Poutine deployment and a peers ConfigMap exists (i.e. peers non-empty),
  add a ConfigMap volume + read-only VolumeMount at `/app/config/peers.yaml` (subPath mount).
  Follow the same pattern used for Prowlarr's custom definitions ConfigMap mount.

### `docs/examples/navidrome.yaml`

Minimal example with NFS music mount.

### `docs/examples/poutine.yaml`

Example showing `appConfig.poutine.peers` with one peer entry, plus NFS music reference.

### `README.md`

Add Navidrome and Poutine to the Supported Applications table.

### `entities.json`

Add `"Navidrome"` and `"Poutine"` to `app_types`.

## Tests

### `crates/servarr-crds/tests/defaults_tests.rs`

- `test_navidrome_defaults` — verify port, image, tier, ND_* env vars present.
- `test_poutine_defaults` — verify port, image, tier, `/app/data` PVC, NODE_ENV + path env vars.

### `crates/servarr-resources/tests/builder_tests.rs`

- `test_poutine_peers_configmap` — build a Poutine app with two peers; assert ConfigMap
  contains `peers.yaml` key with correct YAML content.
- `test_poutine_no_peers_no_configmap` — build a Poutine app with empty peers; assert
  `build_poutine_peers` returns `None`.

## Out of Scope

- Navidrome `AppConfig` — no operator-managed config fields for Navidrome in this iteration.
  Credentials and scan settings are left to env vars on the CR.
- Poutine admin credentials via `adminCredentials` — can be added in a follow-up once the
  Poutine API is known.
- Automatic Poutine↔Navidrome wiring (analogous to Prowlarr sync) — out of scope; user
  sets `NAVIDROME_URL` etc. via env overrides on the CR.
