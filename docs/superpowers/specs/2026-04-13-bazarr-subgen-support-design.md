# Bazarr and Subgen Support Design

**Date:** 2026-04-13  
**Issues:** [#18](https://github.com/phaedrus1992/servarr-operator/issues/18) (Bazarr), [#19](https://github.com/phaedrus1992/servarr-operator/issues/19) (Subgen)

---

## Overview

Add Bazarr (subtitle management) and Subgen (Whisper AI subtitle generation) as supported apps in the servarr-operator. Both are wired as "dumb apps" following the existing cross-app sync pattern used by Prowlarr and Overseerr: the operator discovers companion apps in the namespace and configures the integration via live API calls.

---

## Bazarr

### Image defaults

```toml
[bazarr]
repository = "linuxserver/bazarr"
tag = "1.5.6"
port = 6767
security = "linuxserver"
downloads = false
probe_path = "/api/system/ping"
```

### AppType

Add `Bazarr` to the `AppType` enum in `crates/servarr-crds/src/v1alpha1/spec.rs`:

- `as_str()` → `"bazarr"`
- `tier()` → `3` (Ancillary — starts after Sonarr/Radarr/Lidarr are ready)

### Credential management

Bazarr does **not** support env var injection for its API key. The key is auto-generated on first boot and stored in `/config/config/config.yaml` (Dynaconf YAML backend).

The operator uses an **init-container bootstrap** (identical pattern to Transmission's `apply-settings.sh`):

1. On first reconcile, the operator generates a random 32-character hex API key and stores it in an operator-managed Secret (`<name>-api-key`) in the app's namespace. If the Secret already exists, the existing key is used (idempotent).
2. An init container (using the same `linuxserver/bazarr` image) runs before the main container. It mounts the Secret and writes `/config/config/config.yaml` if the file does not already exist:
   ```yaml
   general:
     apikey: <api-key-from-secret>
   auth:
     type: form
     username: <username>
     password: <md5(password)>   # only written if adminCredentials is set
   ```
3. The main Bazarr container starts, reads the pre-written config, and uses the operator-provisioned key.
4. `bazarr_sync` and `sync_admin_credentials` read the key from the managed Secret for all subsequent API calls.

This eliminates any need for the user to manually seed the API key. The `apiKeySecret` field is **not** exposed on `ServarrAppSpec` for Bazarr — the operator manages the secret entirely.

Admin credential management (username/password) is handled via `sync_admin_credentials` (Path B — live API calls on every reconcile):
- `POST /api/system/settings` with form data `settings-auth-type=form`, `settings-auth-username=<user>`, `settings-auth-password=<md5(password)>`
- Auth header: `X-API-KEY: <key>`
- Returns `204 No Content` on success

This is gated on `adminCredentials` being set; if not set, the operator skips credential management (and omits the auth block from the init-container config write).

### Config written by init container

The init container script (`bazarr-init.sh`) follows the same structure as `apply-settings.sh` for Transmission:

```sh
#!/bin/sh
set -eu
CONFIG=/config/config/config.yaml
if [ -f "$CONFIG" ]; then
  echo "Bazarr config already exists, skipping init"
  exit 0
fi
mkdir -p "$(dirname "$CONFIG")"
cat > "$CONFIG" << EOF
general:
  apikey: ${BAZARR_API_KEY}
auth:
  type: ${BAZARR_AUTH_TYPE:-noauth}
  username: ${BAZARR_USERNAME:-}
  password: ${BAZARR_PASSWORD_MD5:-}
EOF
```

Env vars injected from Secrets/ConfigMaps by the operator. The `bazarr-init.sh` script is stored as a ConfigMap and mounted into the init container.

### Cross-app sync (`BazarrSyncSpec`)

New optional field on `ServarrAppSpec`:

```rust
pub bazarr_sync: Option<BazarrSyncSpec>,
```

```rust
pub struct BazarrSyncSpec {
    pub enabled: bool,
    pub namespace_scope: Option<String>,  // defaults to Bazarr CR's namespace
    pub auto_remove: bool,                // default true
}
```

When `enabled: true`, on every reconcile the operator:

1. Lists all `ServarrApp` CRs in the target namespace
2. For each Sonarr instance found: calls `POST /api/system/settings` on Bazarr with:
   - `settings-general-use_sonarr=true`
   - `settings-sonarr-ip=<sonarr-service-name>.<namespace>.svc.cluster.local`
   - `settings-sonarr-port=8989`
   - `settings-sonarr-base_url=/`
   - `settings-sonarr-ssl=false`
   - `settings-sonarr-apikey=<sonarr-api-key-from-managed-secret>`
3. For each Radarr instance found: same pattern with Radarr fields
4. If `autoRemove: true` and a previously-synced app no longer has a CR, calls the settings endpoint to disable it (`settings-general-use_sonarr=false` etc.)

Sonarr/Radarr API keys are read from their respective operator-managed `<name>-api-key` Secrets. If a companion app has no managed secret, it is skipped with a warning.

The sync function follows the same structure as `sync_overseerr_servers` in `controller.rs`.

### New API client: `crates/servarr-api/src/bazarr.rs`

```rust
pub struct BazarrClient { /* base_url, api_key, http_client */ }

impl BazarrClient {
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ...>;
    pub async fn ping(&self) -> Result<(), ...>;
    pub async fn post_settings(&self, form: &[(&str, &str)]) -> Result<(), ...>;
    pub async fn configure_sonarr(&self, host: &str, port: u16, api_key: &str) -> Result<(), ...>;
    pub async fn configure_radarr(&self, host: &str, port: u16, api_key: &str) -> Result<(), ...>;
    pub async fn disable_sonarr(&self) -> Result<(), ...>;
    pub async fn disable_radarr(&self) -> Result<(), ...>;
    pub async fn set_credentials(&self, username: &str, password_md5: &str) -> Result<(), ...>;
}
```

`set_credentials` takes the MD5-hashed password (the caller computes `md5(plaintext)`).

---

## Subgen

### Image defaults

```toml
[subgen]
repository = "mccloud/subgen"
tag = "2026.04.3"
port = 9000
security = "nonroot"
downloads = false
probe_path = "/status"
```

The GPU-capable image (`mccloud/subgen`, no `-cpu` suffix) is used. It runs on CPU by default (`TRANSCRIBE_DEVICE=cpu`). Users with Nvidia nodes set `spec.gpu` + `spec.env: [{name: TRANSCRIBE_DEVICE, value: cuda}]`.

### AppType

Add `Subgen` to the `AppType` enum:

- `as_str()` → `"subgen"`
- `tier()` → `0` (Infrastructure/Media Servers — should be ready when Jellyfin starts adding media via webhooks)

### Volumes

Subgen needs a second PVC for Whisper model storage (models are 1–3 GB and should persist across pod restarts). This is handled app-specifically in `defaults.rs` rather than via the TOML `downloads` flag:

```rust
if matches!(app, super::AppType::Subgen) {
    defaults.persistence.volumes.push(pvc("models", "/subgen/models", "10Gi"));
}
```

The config PVC (`/config`, 1 Gi) is already provisioned by `nonroot_base`.

### Default env vars

The `nonroot_base` constructor gets two extra env vars for Subgen:

```rust
if matches!(app, super::AppType::Subgen) {
    defaults.env.extend([
        EnvVar { name: "TRANSCRIBE_DEVICE".into(), value: "cpu".into() },
        EnvVar { name: "WHISPER_MODEL".into(),    value: "medium".into() },
    ]);
}
```

Users override these via `spec.env` in the CR.

### No credential management

Subgen has no authentication. It does not appear in `sync_admin_credentials`. The `_ => return None` arm handles it.

### Cross-app sync (`SubgenSyncSpec`)

New optional field on `ServarrAppSpec`:

```rust
pub subgen_sync: Option<SubgenSyncSpec>,
```

```rust
pub struct SubgenSyncSpec {
    pub enabled: bool,
    pub namespace_scope: Option<String>,  // defaults to Subgen CR's namespace
}
```

When `enabled: true`, on every reconcile the operator:

1. Discovers Jellyfin `ServarrApp` CRs in the target namespace
2. Takes the first Jellyfin instance found (multiple Jellyfin instances are unsupported for now)
3. Reads Jellyfin's operator-managed `<jellyfin-name>-api-key` Secret to get the API token
4. Injects two env vars into the Subgen Deployment (via patch if not already present):
   - `JELLYFIN_SERVER=http://<jellyfin-service>.<namespace>.svc.cluster.local:8096`
   - `JELLYFIN_TOKEN=<jellyfin-api-key>`
5. If no Jellyfin instance is found (or Jellyfin has no managed secret), logs a warning and skips

Unlike Bazarr sync (which calls an API), Subgen sync works by adding env vars to the Deployment spec during the normal Deployment reconcile. The `sync_subgen_jellyfin` function runs before Deployment creation/update and returns the env vars to inject; the Deployment builder merges them into the container env. This means a change in Jellyfin's API key triggers a Deployment update (and pod restart) for Subgen automatically.

There is no `autoRemove` for Subgen sync: if Jellyfin disappears, the env vars remain (stale but harmless) until the user re-reconciles or removes `subgen_sync`.

No new API client is needed for Subgen sync.

---

## Files to create or modify

| File | Change |
|------|--------|
| `image-defaults.toml` | Add `[bazarr]` and `[subgen]` sections |
| `crates/servarr-crds/src/v1alpha1/spec.rs` | Add `Bazarr`, `Subgen` to `AppType`; add `bazarr_sync`, `subgen_sync` fields to `ServarrAppSpec` |
| `crates/servarr-crds/src/v1alpha1/types.rs` | Add `BazarrSyncSpec`, `SubgenSyncSpec` structs |
| `crates/servarr-crds/src/v1alpha1/defaults.rs` | Add Subgen models PVC and default env vars; add Bazarr init-container config write |
| `crates/servarr-api/src/bazarr.rs` | New `BazarrClient` |
| `crates/servarr-api/src/lib.rs` | `pub mod bazarr; pub use bazarr::BazarrClient;` |
| `crates/servarr-operator/src/controller.rs` | Add `sync_admin_credentials` arm for Bazarr; add `sync_bazarr_apps`, `sync_subgen_jellyfin`, `ensure_api_key_secret` functions; wire them in reconcile loop |
| `crates/servarr-resources/src/configmap.rs` | Add `bazarr_init_script()` returning the `bazarr-init.sh` ConfigMap |
| `crates/servarr-resources/src/deployment.rs` | Wire Bazarr init container (mounts ConfigMap + Secret); wire Subgen env var injection |
| `charts/servarr-operator/values.yaml` | Regenerate via `scripts/sync-image-defaults.sh` |
| `charts/servarr-crds/templates/servarrapp-crd.yaml` | Regenerate via `scripts/generate-crds.sh` |
| `charts/servarr-crds/templates/mediastack-crd.yaml` | Regenerate via `scripts/generate-crds.sh` |
| `.github/smoke-test/manifests/bazarr.yaml` | New minimal `ServarrApp` CR |
| `.github/smoke-test/manifests/subgen.yaml` | New minimal `ServarrApp` CR |
| `.github/smoke-test/smoke-test.sh` | Add `[bazarr]=6767` and `[subgen]=9000` to `APP_PORTS` |
| `crates/servarr-crds/tests/defaults_tests.rs` | Tests for Bazarr and Subgen defaults |

---

## Out of scope

- Bazarr subtitle provider configuration (Opensubtitles, etc.) — user-configured post-deploy
- Bazarr language profile setup — user-configured post-deploy
- Subgen → Plex integration — deferred; Plex webhook wiring is different and Plex is less common
- Multiple Jellyfin instances for Subgen sync — first instance wins
- Bazarr → Lidarr integration — Bazarr does not support Lidarr
