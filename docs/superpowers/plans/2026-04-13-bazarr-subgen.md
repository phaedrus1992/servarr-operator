# Bazarr and Subgen Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Bazarr (subtitle management) and Subgen (AI subtitle generation) as fully auto-configured apps in the servarr-operator, with Bazarr auto-syncing to Sonarr/Radarr and Subgen auto-syncing to Jellyfin.

**Architecture:** Bazarr uses an init-container to pre-seed `/config/config/config.yaml` with an operator-managed API key before first boot, then `BazarrClient` calls `/api/system/settings` for cross-app sync and admin credentials. Subgen needs no API client — its Jellyfin wiring is injected as env vars in the Deployment spec during reconcile.

**Tech Stack:** Rust, kube-rs, k8s-openapi, reqwest, thiserror, serde/serde_json

---

## File Map

| File | Change |
|------|--------|
| `image-defaults.toml` | Add `[bazarr]` and `[subgen]` sections |
| `crates/servarr-crds/src/v1alpha1/spec.rs` | Add `Bazarr`, `Subgen` enum variants |
| `crates/servarr-crds/src/v1alpha1/types.rs` | Add `BazarrSyncSpec`, `SubgenSyncSpec` structs |
| `crates/servarr-crds/src/v1alpha1/spec.rs` | Add `bazarr_sync`, `subgen_sync` fields to `ServarrAppSpec` |
| `crates/servarr-crds/src/v1alpha1/defaults.rs` | Subgen models PVC + default env vars; Bazarr has no special defaults |
| `crates/servarr-api/src/bazarr.rs` | New `BazarrClient` |
| `crates/servarr-api/src/lib.rs` | Export `BazarrClient` |
| `crates/servarr-resources/src/configmap.rs` | Add `build_bazarr_init()` returning the init ConfigMap |
| `crates/servarr-resources/src/deployment.rs` | Bazarr init container wiring; Subgen env var injection |
| `crates/servarr-operator/src/controller.rs` | `ensure_api_key_secret` Bazarr arm; Bazarr/Subgen sync functions; reconcile wiring |
| `charts/servarr-operator/values.yaml` | Regenerate via `scripts/sync-image-defaults.sh` |
| `charts/servarr-crds/templates/*.yaml` | Regenerate via `scripts/generate-crds.sh` |
| `.github/smoke-test/manifests/bazarr.yaml` | New minimal CR |
| `.github/smoke-test/manifests/subgen.yaml` | New minimal CR |
| `.github/smoke-test/smoke-test.sh` | Add ports to `APP_PORTS` |
| `crates/servarr-crds/tests/defaults_tests.rs` | New tests for Bazarr and Subgen defaults |

---

## Task 1: Add image defaults for Bazarr and Subgen

**Files:**
- Modify: `image-defaults.toml`

- [ ] **Step 1: Add the two new sections to image-defaults.toml**

Open `image-defaults.toml` and add after the `[jackett]` section:

```toml
[bazarr]
repository = "linuxserver/bazarr"
tag = "1.5.6"
port = 6767
security = "linuxserver"
downloads = false
probe_path = "/api/system/ping"

[subgen]
repository = "mccloud/subgen"
tag = "2026.04.3"
port = 9000
security = "nonroot"
downloads = false
probe_path = "/status"
```

- [ ] **Step 2: Verify the TOML parses and build still compiles**

```bash
cargo build -p servarr-crds 2>&1 | head -30
```

Expected: No errors (the `build.rs` codegen parses `image-defaults.toml` at build time and panics if it cannot parse the file).

- [ ] **Step 3: Commit**

```bash
git add image-defaults.toml
git commit -m "feat: add image defaults for bazarr and subgen"
```

---

## Task 2: Add AppType variants

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/spec.rs:117-182`

- [ ] **Step 1: Write the failing test**

Open `crates/servarr-crds/tests/defaults_tests.rs` and add:

```rust
#[test]
fn bazarr_has_correct_tier() {
    assert_eq!(AppType::Bazarr.tier(), 3);
}

#[test]
fn subgen_has_correct_tier() {
    assert_eq!(AppType::Subgen.tier(), 0);
}

#[test]
fn bazarr_as_str() {
    assert_eq!(AppType::Bazarr.as_str(), "bazarr");
}

#[test]
fn subgen_as_str() {
    assert_eq!(AppType::Subgen.as_str(), "subgen");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p servarr-crds bazarr_has_correct_tier 2>&1 | tail -10
```

Expected: compile error — `AppType::Bazarr` does not exist yet.

- [ ] **Step 3: Add the enum variants**

In `crates/servarr-crds/src/v1alpha1/spec.rs` line ~133, add `Bazarr` and `Subgen` to the enum:

```rust
pub enum AppType {
    #[default]
    Sonarr,
    Radarr,
    Lidarr,
    Prowlarr,
    Sabnzbd,
    Transmission,
    Tautulli,
    Overseerr,
    Maintainerr,
    Jackett,
    Jellyfin,
    Plex,
    SshBastion,
    Bazarr,   // add
    Subgen,   // add
}
```

- [ ] **Step 4: Add `as_str()` arms**

In the `as_str()` match (line ~135), add before the closing brace:

```rust
AppType::Bazarr => "bazarr",
AppType::Subgen => "subgen",
```

- [ ] **Step 5: Add `tier()` arms**

In the `tier()` match (line ~160), add:

```rust
// Tier 0 arm (Plex, Jellyfin, SshBastion) — add Subgen here
AppType::Plex | AppType::Jellyfin | AppType::SshBastion | AppType::Subgen => 0,
// Tier 3 arm (Tautulli, Overseerr, Maintainerr, Prowlarr, Jackett) — add Bazarr here
AppType::Tautulli | AppType::Overseerr | AppType::Maintainerr | AppType::Prowlarr
    | AppType::Jackett | AppType::Bazarr => 3,
```

- [ ] **Step 6: Fix the non-exhaustive `app_type_to_kind()` in controller.rs**

In `crates/servarr-operator/src/controller.rs` line 27, the function panics on unsupported types — Bazarr and Subgen should fall through to the existing `other => panic!(...)` arm, which is fine. Verify the `needs_rollout_on_secret_change` match at line ~381 does NOT need updating (it already uses an explicit list, so Bazarr and Subgen will correctly be excluded).

- [ ] **Step 7: Run the tests**

```bash
cargo test -p servarr-crds 2>&1 | tail -20
```

Expected: All 4 new tests pass plus existing tests still pass.

- [ ] **Step 8: Commit**

```bash
git add crates/servarr-crds/src/v1alpha1/spec.rs crates/servarr-crds/tests/defaults_tests.rs
git commit -m "feat: add Bazarr and Subgen to AppType enum"
```

---

## Task 3: Add sync spec types and ServarrAppSpec fields

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/types.rs`
- Modify: `crates/servarr-crds/src/v1alpha1/spec.rs`

- [ ] **Step 1: Write the failing test**

In `crates/servarr-crds/tests/defaults_tests.rs` add:

```rust
#[test]
fn bazarr_sync_spec_defaults() {
    use servarr_crds::v1alpha1::types::BazarrSyncSpec;
    let spec: BazarrSyncSpec = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
    assert!(spec.enabled);
    assert!(spec.auto_remove); // default true
    assert!(spec.namespace_scope.is_none());
}

#[test]
fn subgen_sync_spec_defaults() {
    use servarr_crds::v1alpha1::types::SubgenSyncSpec;
    let spec: SubgenSyncSpec = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
    assert!(spec.enabled);
    assert!(spec.namespace_scope.is_none());
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p servarr-crds bazarr_sync_spec_defaults 2>&1 | tail -10
```

Expected: compile error — type does not exist.

- [ ] **Step 3: Add sync spec structs to types.rs**

Open `crates/servarr-crds/src/v1alpha1/types.rs`. After the `OverseerrSyncSpec` definition (around line 509), add:

```rust
/// Sync spec for Bazarr → Sonarr/Radarr integration.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BazarrSyncSpec {
    /// Enable Bazarr cross-app sync.
    #[serde(default)]
    pub enabled: bool,
    /// Namespace to discover companion apps in. Defaults to Bazarr's own namespace.
    #[serde(default)]
    pub namespace_scope: Option<String>,
    /// Remove Sonarr/Radarr registrations from Bazarr when their CRs disappear.
    #[serde(default = "default_true")]
    pub auto_remove: bool,
}

/// Sync spec for Subgen → Jellyfin integration.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubgenSyncSpec {
    /// Enable Subgen cross-app sync with Jellyfin.
    #[serde(default)]
    pub enabled: bool,
    /// Namespace to discover Jellyfin in. Defaults to Subgen's own namespace.
    #[serde(default)]
    pub namespace_scope: Option<String>,
}
```

Note: `default_true` is already defined in this file (used by `ProwlarrSyncSpec` and `OverseerrSyncSpec`).

- [ ] **Step 4: Add the fields to ServarrAppSpec**

In `crates/servarr-crds/src/v1alpha1/spec.rs`, after the `overseerr_sync` field (around line 105), add:

```rust
/// Bazarr cross-app sync configuration (only used when app = Bazarr).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub bazarr_sync: Option<BazarrSyncSpec>,

/// Subgen → Jellyfin sync configuration (only used when app = Subgen).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub subgen_sync: Option<SubgenSyncSpec>,
```

Also add the imports at the top of `spec.rs` if not already present (check the existing use statements — `BazarrSyncSpec` and `SubgenSyncSpec` need to be in scope). The pattern used by `ProwlarrSyncSpec` shows these come from `super::types::*` or are re-exported from the module — follow the existing import pattern.

- [ ] **Step 5: Run the tests**

```bash
cargo test -p servarr-crds 2>&1 | tail -20
```

Expected: Both new tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/servarr-crds/src/v1alpha1/types.rs crates/servarr-crds/src/v1alpha1/spec.rs crates/servarr-crds/tests/defaults_tests.rs
git commit -m "feat: add BazarrSyncSpec, SubgenSyncSpec and wire into ServarrAppSpec"
```

---

## Task 4: Add Subgen-specific defaults (models PVC + env vars)

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/defaults.rs`
- Modify: `crates/servarr-crds/tests/defaults_tests.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn subgen_has_models_pvc() {
    let defaults = AppDefaults::for_app(&AppType::Subgen);
    let has_models = defaults
        .persistence
        .volumes
        .iter()
        .any(|v| v.name == "models" && v.mount_path == "/subgen/models");
    assert!(has_models, "Subgen should have a 'models' PVC at /subgen/models");
}

#[test]
fn subgen_default_env_includes_transcribe_device() {
    let defaults = AppDefaults::for_app(&AppType::Subgen);
    let has_device = defaults.env.iter().any(|e| {
        e.name == "TRANSCRIBE_DEVICE" && e.value == "cpu"
    });
    assert!(has_device, "Subgen should default TRANSCRIBE_DEVICE=cpu");
}

#[test]
fn subgen_default_env_includes_whisper_model() {
    let defaults = AppDefaults::for_app(&AppType::Subgen);
    let has_model = defaults.env.iter().any(|e| {
        e.name == "WHISPER_MODEL" && e.value == "medium"
    });
    assert!(has_model, "Subgen should default WHISPER_MODEL=medium");
}

#[test]
fn bazarr_defaults_are_linuxserver_profile() {
    let defaults = AppDefaults::for_app(&AppType::Bazarr);
    assert!(matches!(
        defaults.security.profile_type,
        servarr_crds::v1alpha1::types::SecurityProfileType::LinuxServer
    ));
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p servarr-crds subgen_has_models_pvc 2>&1 | tail -10
```

Expected: FAIL (no models PVC yet).

- [ ] **Step 3: Add Subgen-specific overrides to `for_app()`**

In `crates/servarr-crds/src/v1alpha1/defaults.rs`, inside `for_app()` after the `if matches!(app, super::AppType::Transmission)` block, add:

```rust
if matches!(app, super::AppType::Subgen) {
    defaults.persistence.volumes.push(pvc("models", "/subgen/models", "10Gi"));
    defaults.env.extend([
        super::types::EnvVar { name: "TRANSCRIBE_DEVICE".into(), value: "cpu".into() },
        super::types::EnvVar { name: "WHISPER_MODEL".into(), value: "medium".into() },
    ]);
}
```

Note: Check the existing `EnvVar` usage in `linuxserver_base` and `nonroot_base` to confirm the correct path — it may be `EnvVar` (if already imported) or `super::types::EnvVar`.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p servarr-crds 2>&1 | tail -20
```

Expected: All 4 new tests pass plus all prior tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/servarr-crds/src/v1alpha1/defaults.rs crates/servarr-crds/tests/defaults_tests.rs
git commit -m "feat: add Subgen models PVC and default env vars in AppDefaults"
```

---

## Task 5: Add BazarrClient

**Files:**
- Create: `crates/servarr-api/src/bazarr.rs`
- Modify: `crates/servarr-api/src/lib.rs`

Bazarr's relevant API endpoints:
- `GET /api/system/ping` — returns `{"status": "OK"}`, used for health check
- `POST /api/system/settings` — multipart form data (not JSON), used for all settings changes

The `HttpClient` in `client.rs` sends JSON bodies. Bazarr uses form-encoded `POST` bodies, so `BazarrClient` uses `reqwest` directly.

- [ ] **Step 1: Create `crates/servarr-api/src/bazarr.rs`**

```rust
//! Bazarr API client for subtitle manager configuration.

use reqwest::Client;

use crate::ApiError;

/// Client for the Bazarr subtitle management API.
#[derive(Debug, Clone)]
pub struct BazarrClient {
    base_url: String,
    api_key: String,
    http: Client,
}

impl BazarrClient {
    /// Create a new `BazarrClient`.
    ///
    /// # Errors
    ///
    /// Returns `ApiError::InvalidApiKey` if `api_key` contains non-visible-ASCII characters.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        if api_key.bytes().any(|b| b < 0x21 || b > 0x7e) {
            return Err(ApiError::InvalidApiKey);
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            http: Client::new(),
        })
    }

    /// Ping the Bazarr health endpoint.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` if the request fails or the server returns a non-2xx status.
    pub async fn ping(&self) -> Result<(), ApiError> {
        let url = format!("{}/api/system/ping", self.base_url);
        let resp = self
            .http
            .get(&url)
            .header("X-API-KEY", &self.api_key)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// POST form data to `/api/system/settings`.
    ///
    /// Bazarr settings are form-encoded, not JSON. The caller assembles the form fields.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` if the request fails or the server returns a non-2xx status.
    pub async fn post_settings(&self, form: &[(&str, &str)]) -> Result<(), ApiError> {
        let url = format!("{}/api/system/settings", self.base_url);
        let resp = self
            .http
            .post(&url)
            .header("X-API-KEY", &self.api_key)
            .form(form)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Configure Bazarr to use a Sonarr instance.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn configure_sonarr(
        &self,
        host: &str,
        port: u16,
        api_key: &str,
    ) -> Result<(), ApiError> {
        self.post_settings(&[
            ("settings-general-use_sonarr", "true"),
            ("settings-sonarr-ip", host),
            ("settings-sonarr-port", &port.to_string()),
            ("settings-sonarr-base_url", "/"),
            ("settings-sonarr-ssl", "false"),
            ("settings-sonarr-apikey", api_key),
        ])
        .await
    }

    /// Configure Bazarr to use a Radarr instance.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn configure_radarr(
        &self,
        host: &str,
        port: u16,
        api_key: &str,
    ) -> Result<(), ApiError> {
        self.post_settings(&[
            ("settings-general-use_radarr", "true"),
            ("settings-radarr-ip", host),
            ("settings-radarr-port", &port.to_string()),
            ("settings-radarr-base_url", "/"),
            ("settings-radarr-ssl", "false"),
            ("settings-radarr-apikey", api_key),
        ])
        .await
    }

    /// Disable Sonarr in Bazarr.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn disable_sonarr(&self) -> Result<(), ApiError> {
        self.post_settings(&[("settings-general-use_sonarr", "false")])
            .await
    }

    /// Disable Radarr in Bazarr.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn disable_radarr(&self) -> Result<(), ApiError> {
        self.post_settings(&[("settings-general-use_radarr", "false")])
            .await
    }

    /// Set Bazarr admin credentials (form login).
    ///
    /// `password_md5` must be the MD5 hex digest of the plaintext password — Bazarr
    /// stores and compares the MD5 hash, not the plaintext.
    ///
    /// # Errors
    ///
    /// Returns `ApiError` on request failure or non-2xx response.
    pub async fn set_credentials(
        &self,
        username: &str,
        password_md5: &str,
    ) -> Result<(), ApiError> {
        self.post_settings(&[
            ("settings-auth-type", "form"),
            ("settings-auth-username", username),
            ("settings-auth-password", password_md5),
        ])
        .await
    }
}
```

- [ ] **Step 2: Export from lib.rs**

Open `crates/servarr-api/src/lib.rs` and add alongside the other module declarations:

```rust
pub mod bazarr;
pub use bazarr::BazarrClient;
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo build -p servarr-api 2>&1 | head -30
```

Expected: No errors.

- [ ] **Step 4: Commit**

```bash
git add crates/servarr-api/src/bazarr.rs crates/servarr-api/src/lib.rs
git commit -m "feat: add BazarrClient for subtitle manager API"
```

---

## Task 6: Add Bazarr init container ConfigMap

**Files:**
- Modify: `crates/servarr-resources/src/configmap.rs`

The init script is stored as a ConfigMap (same pattern as `build_transmission()` which stores `apply-settings.sh`). The init container runs this script to write `/config/config/config.yaml` before Bazarr boots.

- [ ] **Step 1: Add `build_bazarr_init()` to configmap.rs**

Open `crates/servarr-resources/src/configmap.rs`. After the existing `build_transmission()` function, add:

```rust
/// Build the ConfigMap containing the Bazarr init script.
///
/// The init script writes `/config/config/config.yaml` if it does not already exist,
/// seeding Bazarr with the operator-managed API key (and optional auth config).
/// This is idempotent: a second run is a no-op if the file exists.
pub fn build_bazarr_init(app: &ServarrApp) -> Option<ConfigMap> {
    if !matches!(app.spec.app, AppType::Bazarr) {
        return None;
    }

    let name = common::child_name(app, "init");
    let ns = common::app_namespace(app);

    // The script is intentionally simple — no jq dependency.
    // BAZARR_API_KEY comes from the operator-managed Secret.
    // BAZARR_AUTH_TYPE / BAZARR_USERNAME / BAZARR_PASSWORD_MD5 come from
    // a projected volume when adminCredentials is set; they may be empty strings.
    let script = r#"#!/bin/sh
set -eu
CONFIG=/config/config/config.yaml
if [ -f "$CONFIG" ]; then
  echo "bazarr-init: config already exists, skipping"
  exit 0
fi
mkdir -p "$(dirname "$CONFIG")"
cat > "$CONFIG" << BAZARR_EOF
general:
  apikey: ${BAZARR_API_KEY}
auth:
  type: ${BAZARR_AUTH_TYPE:-noauth}
  username: ${BAZARR_USERNAME:-}
  password: ${BAZARR_PASSWORD_MD5:-}
BAZARR_EOF
echo "bazarr-init: wrote $CONFIG"
"#;

    Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(ns),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            "bazarr-init.sh".to_string(),
            script.to_string(),
        )])),
        ..Default::default()
    })
}
```

Check the existing imports at the top of `configmap.rs` — `ConfigMap`, `ObjectMeta`, `BTreeMap`, `AppType`, `ServarrApp`, and the `common` module should already be imported. If not, follow the same import pattern used by `build_transmission()`.

- [ ] **Step 2: Verify it compiles**

```bash
cargo build -p servarr-resources 2>&1 | head -30
```

Expected: No errors.

- [ ] **Step 3: Commit**

```bash
git add crates/servarr-resources/src/configmap.rs
git commit -m "feat: add Bazarr init ConfigMap with config pre-seed script"
```

---

## Task 7: Wire Bazarr init container and Subgen env injection in the Deployment builder

**Files:**
- Modify: `crates/servarr-resources/src/deployment.rs`

Two changes in this task:
1. Bazarr gets an `apply-settings` init container that runs `bazarr-init.sh`
2. Subgen env vars (`JELLYFIN_SERVER`, `JELLYFIN_TOKEN`) are accepted as extra env vars passed from the controller (see Task 9)

For Subgen, the Deployment builder already accepts user-defined `spec.env` — the controller will pass Jellyfin env vars as additional entries through the existing env-merge path. No structural changes to `build()` are needed for Subgen; the Deployment builder already picks up all `spec.env` entries. This task only handles the Bazarr init container.

- [ ] **Step 1: Add Bazarr init container after the Transmission init container block**

In `crates/servarr-resources/src/deployment.rs`, find the Transmission init container block (around line 1040). After its closing `}`, add:

```rust
if matches!(app.spec.app, AppType::Bazarr) {
    let init_sec = SecurityContext {
        run_as_user: Some(uid),
        run_as_group: Some(gid),
        ..security_context.clone()
    };
    let mut bazarr_init_mounts = vec![
        VolumeMount {
            name: "config".into(),
            mount_path: "/config".into(),
            ..Default::default()
        },
        VolumeMount {
            name: "bazarr-init-scripts".into(),
            mount_path: "/scripts".into(),
            read_only: Some(true),
            ..Default::default()
        },
        VolumeMount {
            name: "bazarr-api-key".into(),
            mount_path: "/run/secrets/api-key".into(),
            read_only: Some(true),
            ..Default::default()
        },
    ];
    if app.spec.admin_credentials.is_some() {
        bazarr_init_mounts.push(VolumeMount {
            name: "admin-credentials".into(),
            mount_path: "/run/secrets/admin".into(),
            read_only: Some(true),
            ..Default::default()
        });
    }
    init.push(Container {
        name: "bazarr-init".into(),
        image: Some(image.to_string()),
        command: Some(vec![
            "/bin/sh".into(),
            "/scripts/bazarr-init.sh".into(),
        ]),
        env: Some(vec![
            // API key read from file (Secret-mounted)
            k8s_openapi::api::core::v1::EnvVar {
                name: "BAZARR_API_KEY".into(),
                value_from: Some(k8s_openapi::api::core::v1::EnvVarSource {
                    secret_key_ref: Some(k8s_openapi::api::core::v1::SecretKeySelector {
                        name: common::child_name(app, "api-key"),
                        key: "api-key".into(),
                        optional: Some(false),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]),
        security_context: Some(init_sec),
        volume_mounts: Some(bazarr_init_mounts),
        ..Default::default()
    });
}
```

- [ ] **Step 2: Add the Bazarr init ConfigMap volume to the volumes list**

In `deployment.rs`, find where volumes are assembled (look for where the `scripts` volume is added for Transmission — it's a `ConfigMap` volume projection). After the Transmission scripts volume block, add:

```rust
if matches!(app.spec.app, AppType::Bazarr) {
    volumes.push(Volume {
        name: "bazarr-init-scripts".into(),
        config_map: Some(ConfigMapVolumeSource {
            name: common::child_name(app, "init"),
            default_mode: Some(0o755),
            ..Default::default()
        }),
        ..Default::default()
    });
    volumes.push(Volume {
        name: "bazarr-api-key".into(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(common::child_name(app, "api-key")),
            ..Default::default()
        }),
        ..Default::default()
    });
}
```

- [ ] **Step 3: Verify compilation**

```bash
cargo build -p servarr-resources 2>&1 | head -30
```

Expected: No errors. If you see "use of undeclared type" errors, add the missing k8s-openapi types to the imports section at the top of `deployment.rs` using the same `use` path as `ConfigMapVolumeSource` and `SecretVolumeSource` already used in the file.

- [ ] **Step 4: Commit**

```bash
git add crates/servarr-resources/src/deployment.rs
git commit -m "feat: wire Bazarr init container in deployment builder"
```

---

## Task 8: Extend ensure_api_key_secret for Bazarr

**Files:**
- Modify: `crates/servarr-operator/src/controller.rs:577-608`

Currently `ensure_api_key_secret` returns `Ok(())` immediately if `api_key_secret` is `None`. For Bazarr, the operator always manages the API key secret (named `<app-name>-api-key`) regardless of whether the user sets `api_key_secret`. We add a Bazarr-specific arm that auto-creates this secret.

- [ ] **Step 1: Extend `ensure_api_key_secret` with a Bazarr arm**

Replace the current `ensure_api_key_secret` function body (lines 577–608) with:

```rust
async fn ensure_api_key_secret(client: &Client, app: &ServarrApp, ns: &str) -> Result<(), Error> {
    // For Bazarr, the operator always manages the API key secret using a
    // deterministic name (<app-name>-api-key), regardless of apiKeySecret spec.
    let (secret_name, use_child_name) = if matches!(app.spec.app, AppType::Bazarr) {
        (
            servarr_resources::common::child_name(app, "api-key"),
            true,
        )
    } else {
        match app.spec.api_key_secret.as_deref() {
            Some(s) => (s.to_string(), false),
            None => return Ok(()),
        }
    };

    let secret_api = Api::<Secret>::namespaced(client.clone(), ns);

    // Only create if the Secret does not already exist.
    match secret_api.get(&secret_name).await {
        Ok(_) => return Ok(()),
        Err(kube::Error::Api(err)) if err.code == 404 => {}
        Err(e) => return Err(Error::Kube(e)),
    }

    use rand::Rng as _;
    let key: String = rand::rng()
        .sample_iter(rand::distr::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let secret = if use_child_name {
        // Build the secret directly for Bazarr (child_name-based, no api_key_secret field)
        use k8s_openapi::api::core::v1::Secret;
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
        use std::collections::BTreeMap;
        Secret {
            metadata: ObjectMeta {
                name: Some(secret_name.clone()),
                namespace: Some(ns.to_string()),
                labels: Some(servarr_resources::common::labels(app)),
                owner_references: Some(vec![servarr_resources::common::owner_reference(app)]),
                ..Default::default()
            },
            string_data: Some(BTreeMap::from([("api-key".into(), key)])),
            type_: Some("Opaque".into()),
            ..Default::default()
        }
    } else if let Some(s) = servarr_resources::secret::build_api_key(app, &key) {
        s
    } else {
        return Ok(());
    };

    info!(name = %app.name_any(), secret = %secret_name, "creating api-key secret");
    secret_api
        .create(&PostParams::default(), &secret)
        .await
        .map_err(Error::Kube)?;

    Ok(())
}
```

- [ ] **Step 2: Verify compilation**

```bash
cargo build -p servarr-operator 2>&1 | head -40
```

Expected: No errors.

- [ ] **Step 3: Commit**

```bash
git add crates/servarr-operator/src/controller.rs
git commit -m "feat: auto-manage Bazarr API key secret in ensure_api_key_secret"
```

---

## Task 9: Add sync_bazarr_apps and sync_subgen_jellyfin in the controller

**Files:**
- Modify: `crates/servarr-operator/src/controller.rs`

This is the largest task. We add two sync functions and update the reconcile loop.

### sync_bazarr_apps

Reads the Bazarr app's operator-managed API key, then for each Sonarr/Radarr in the target namespace, calls `BazarrClient::configure_sonarr` / `configure_radarr`. If `auto_remove` is true and a previously-synced type is no longer present, calls the disable method.

### sync_subgen_jellyfin

Discovers Jellyfin in the target namespace, reads its managed API key secret, then returns two `EnvVar` entries to inject into the Subgen Deployment. These env vars are then patched onto the Deployment via SSA.

- [ ] **Step 1: Add `sync_bazarr_apps` after `sync_overseerr_servers`**

After the closing brace of `sync_overseerr_servers` (around line 2100+), add:

```rust
/// Sync Bazarr's Sonarr/Radarr integration via POST /api/system/settings.
///
/// Called on every reconcile when `bazarr_sync.enabled` is true.
async fn sync_bazarr_apps(
    client: &Client,
    bazarr: &ServarrApp,
    target_ns: &str,
    _recorder: &kube::runtime::events::Recorder,
    _obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    let bazarr_name = bazarr.name_any();
    let ns = bazarr.namespace().unwrap_or_else(|| "default".into());

    // Read Bazarr's operator-managed API key
    let api_key_secret = servarr_resources::common::child_name(bazarr, "api-key");
    let bazarr_key =
        servarr_api::read_secret_key(client, &ns, &api_key_secret, "api-key").await?;

    let bazarr_app_name = servarr_resources::common::app_name(bazarr);
    let defaults = servarr_crds::AppDefaults::for_app(&bazarr.spec.app);
    let svc_spec = bazarr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let bazarr_url = format!("http://{bazarr_app_name}.{ns}.svc:{port}");

    let bazarr_client = servarr_api::BazarrClient::new(&bazarr_url, &bazarr_key)?;

    let auto_remove = bazarr
        .spec
        .bazarr_sync
        .as_ref()
        .map(|s| s.auto_remove)
        .unwrap_or(true);

    // Discover Sonarr and Radarr apps in the target namespace
    let discovered = discover_namespace_apps(client, target_ns).await?;

    let has_sonarr = discovered.iter().any(|a| a.app_type == AppType::Sonarr);
    let has_radarr = discovered.iter().any(|a| a.app_type == AppType::Radarr);

    for app in &discovered {
        let companion_defaults = servarr_crds::AppDefaults::for_app(&app.app_type);
        let companion_svc = companion_defaults.service;
        let companion_port = companion_svc.ports.first().map(|p| p.port).unwrap_or(80);
        // Build the in-cluster hostname (service name = app CR name)
        let url = url::Url::parse(&app.base_url)
            .map_err(|e| anyhow::anyhow!("invalid companion URL {}: {e}", app.base_url))?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("no host in {}", app.base_url))?
            .to_string();

        match app.app_type {
            AppType::Sonarr => {
                info!(bazarr = %bazarr_name, sonarr = %app.name, "syncing Sonarr into Bazarr");
                if let Err(e) = bazarr_client
                    .configure_sonarr(&host, companion_port as u16, &app.api_key)
                    .await
                {
                    warn!(bazarr = %bazarr_name, sonarr = %app.name, error = %e, "failed to configure Sonarr in Bazarr");
                }
            }
            AppType::Radarr => {
                info!(bazarr = %bazarr_name, radarr = %app.name, "syncing Radarr into Bazarr");
                if let Err(e) = bazarr_client
                    .configure_radarr(&host, companion_port as u16, &app.api_key)
                    .await
                {
                    warn!(bazarr = %bazarr_name, radarr = %app.name, error = %e, "failed to configure Radarr in Bazarr");
                }
            }
            _ => {}
        }
    }

    // If auto_remove is enabled and a type is absent, disable it in Bazarr
    if auto_remove {
        if !has_sonarr {
            if let Err(e) = bazarr_client.disable_sonarr().await {
                warn!(bazarr = %bazarr_name, error = %e, "failed to disable Sonarr in Bazarr");
            }
        }
        if !has_radarr {
            if let Err(e) = bazarr_client.disable_radarr().await {
                warn!(bazarr = %bazarr_name, error = %e, "failed to disable Radarr in Bazarr");
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 2: Add `sync_subgen_jellyfin`**

After `sync_bazarr_apps`, add:

```rust
/// Patch Jellyfin env vars (JELLYFIN_SERVER, JELLYFIN_TOKEN) onto the Subgen Deployment.
///
/// Called on every reconcile when `subgen_sync.enabled` is true.
/// Returns the two env vars to inject, or an error if Jellyfin is not found.
async fn sync_subgen_jellyfin(
    client: &Client,
    subgen: &ServarrApp,
    target_ns: &str,
) -> Result<(), anyhow::Error> {
    use kube::api::{Api, Patch, PatchParams};

    let subgen_name = subgen.name_any();
    let ns = subgen.namespace().unwrap_or_else(|| "default".into());

    // Find Jellyfin in target namespace
    let all_apps = Api::<ServarrApp>::namespaced(client.clone(), target_ns);
    let app_list = all_apps
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| anyhow::anyhow!("failed to list ServarrApps: {e}"))?;

    let jellyfin = app_list
        .items
        .iter()
        .find(|a| a.spec.app == AppType::Jellyfin);

    let jellyfin = match jellyfin {
        Some(j) => j,
        None => {
            warn!(subgen = %subgen_name, "subgen-sync: no Jellyfin CR found in namespace {target_ns}, skipping");
            return Ok(());
        }
    };

    // Read Jellyfin's operator-managed API key
    let jf_secret_name = servarr_resources::common::child_name(jellyfin, "api-key");
    let jf_key = match servarr_api::read_secret_key(client, target_ns, &jf_secret_name, "api-key")
        .await
    {
        Ok(k) => k,
        Err(e) => {
            warn!(subgen = %subgen_name, error = %e, "subgen-sync: failed to read Jellyfin API key, skipping");
            return Ok(());
        }
    };

    let jf_app_name = servarr_resources::common::app_name(jellyfin);
    let jf_defaults = servarr_crds::AppDefaults::for_app(&jellyfin.spec.app);
    let jf_svc_spec = jellyfin.spec.service.as_ref().unwrap_or(&jf_defaults.service);
    let jf_port = jf_svc_spec.ports.first().map(|p| p.port).unwrap_or(8096);
    let jf_url =
        format!("http://{jf_app_name}.{target_ns}.svc.cluster.local:{jf_port}");

    // Patch the env vars onto the Subgen Deployment via SSA
    let deploy_api = Api::<k8s_openapi::api::apps::v1::Deployment>::namespaced(
        client.clone(),
        &ns,
    );
    let pp = PatchParams::apply("servarr-operator/subgen-jellyfin").force();
    let patch = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": &subgen_name },
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name": &subgen_name,
                        "env": [
                            { "name": "JELLYFIN_SERVER", "value": jf_url },
                            { "name": "JELLYFIN_TOKEN", "value": jf_key },
                        ]
                    }]
                }
            }
        }
    });

    deploy_api
        .patch(&subgen_name, &pp, &Patch::Apply(patch))
        .await
        .map_err(|e| anyhow::anyhow!("failed to patch Subgen Deployment: {e}"))?;

    info!(subgen = %subgen_name, jellyfin = %jf_app_name, "subgen-sync: injected Jellyfin env vars");
    Ok(())
}
```

- [ ] **Step 3: Wire both sync functions into the reconcile loop**

In `reconcile()`, after the Overseerr sync block (around line 503), add:

```rust
// Bazarr cross-app sync (only for Bazarr-type apps with sync enabled)
if app.spec.app == AppType::Bazarr
    && let Some(ref sync_spec) = app.spec.bazarr_sync
    && sync_spec.enabled
{
    let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
    if let Err(e) = sync_bazarr_apps(client, &app, target_ns, &recorder, &obj_ref).await {
        warn!(%name, error = %e, "Bazarr sync failed");
    }
}

// Subgen → Jellyfin sync (only for Subgen-type apps with sync enabled)
if app.spec.app == AppType::Subgen
    && let Some(ref sync_spec) = app.spec.subgen_sync
    && sync_spec.enabled
{
    let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
    if let Err(e) = sync_subgen_jellyfin(client, &app, target_ns).await {
        warn!(%name, error = %e, "Subgen Jellyfin sync failed");
    }
}
```

- [ ] **Step 4: Add Bazarr arm to sync_admin_credentials**

In `sync_admin_credentials()`, before the `_ => return None` arm (around line 877), add:

```rust
AppType::Bazarr => {
    // Read the operator-managed API key for Bazarr
    let api_key_secret = servarr_resources::common::child_name(app, "api-key");
    let api_key = match servarr_api::read_secret_key(client, ns, &api_key_secret, "api-key").await {
        Ok(k) => k,
        Err(e) => {
            return Some(Condition {
                condition_type: condition_types::ADMIN_CREDENTIALS_CONFIGURED.to_string(),
                status: "Unknown".to_string(),
                reason: "ApiKeyReadError".to_string(),
                message: e.to_string(),
                last_transition_time: now,
            });
        }
    };
    match servarr_api::BazarrClient::new(&base_url, &api_key) {
        Ok(c) => {
            use md5::{Digest as _, Md5};
            let hash = Md5::digest(password.as_bytes());
            let password_md5 = format!("{hash:x}");
            c.set_credentials(&username, &password_md5)
                .await
                .map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}
```

Note: This requires the `md5` crate. Add it to `crates/servarr-operator/Cargo.toml`:

```toml
md5 = "0.10"
```

- [ ] **Step 5: Verify compilation**

```bash
cargo build -p servarr-operator 2>&1 | head -50
```

Expected: No errors. If `md5` is not found, run `cargo add md5 -p servarr-operator` to add the dependency, then re-check the version is current stable.

- [ ] **Step 6: Commit**

```bash
git add crates/servarr-operator/src/controller.rs crates/servarr-operator/Cargo.toml Cargo.lock
git commit -m "feat: add sync_bazarr_apps and sync_subgen_jellyfin in controller"
```

---

## Task 10: Wire Bazarr init ConfigMap into the reconcile loop

**Files:**
- Modify: `crates/servarr-operator/src/controller.rs`

The Bazarr init ConfigMap (created in Task 6) must be applied to the cluster during reconcile, just like the Transmission and SABnzbd ConfigMaps.

- [ ] **Step 1: Apply the Bazarr init ConfigMap in reconcile()**

In `reconcile()`, find the block that applies the Prowlarr definitions ConfigMap (around line 357). After it, add:

```rust
// Build and apply Bazarr init ConfigMap (pre-seeds config.yaml before first boot)
if let Some(cm) = servarr_resources::configmap::build_bazarr_init(&app) {
    let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
    let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
    tracing::debug!(%name, cm_name, "SSA: applying Bazarr init ConfigMap");
    cm_api
        .patch(cm_name, &pp, &Patch::Apply(&cm))
        .await
        .map_err(Error::Kube)?;
}
```

- [ ] **Step 2: Verify compilation**

```bash
cargo build -p servarr-operator 2>&1 | head -30
```

Expected: No errors.

- [ ] **Step 3: Run all tests**

```bash
cargo test 2>&1 | tail -30
```

Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/servarr-operator/src/controller.rs
git commit -m "feat: apply Bazarr init ConfigMap during reconcile"
```

---

## Task 11: Regenerate Helm chart artifacts

**Files:**
- Modify: `charts/servarr-operator/values.yaml`
- Modify: `charts/servarr-crds/templates/servarrapp-crd.yaml`
- Modify: `charts/servarr-crds/templates/mediastack-crd.yaml`

- [ ] **Step 1: Regenerate values.yaml**

```bash
bash scripts/sync-image-defaults.sh
```

Expected: `charts/servarr-operator/values.yaml` is updated with `bazarr` and `subgen` image entries.

- [ ] **Step 2: Regenerate CRDs**

```bash
bash scripts/generate-crds.sh
```

Expected: `charts/servarr-crds/templates/servarrapp-crd.yaml` and `mediastack-crd.yaml` are updated with new enum values and new sync spec fields.

- [ ] **Step 3: Verify the CRD output contains the new fields**

```bash
grep -A2 "bazarr\|subgen" charts/servarr-crds/templates/servarrapp-crd.yaml | head -30
```

Expected: You see `bazarr` and `subgen` in the enum values list, plus `bazarrSync` and `subgenSync` in the spec properties.

- [ ] **Step 4: Commit**

```bash
git add charts/
git commit -m "chore: regenerate Helm CRDs and values for Bazarr and Subgen"
```

---

## Task 12: Add smoke test fixtures

**Files:**
- Create: `.github/smoke-test/manifests/bazarr.yaml`
- Create: `.github/smoke-test/manifests/subgen.yaml`
- Modify: `.github/smoke-test/smoke-test.sh`

- [ ] **Step 1: Create bazarr.yaml**

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: bazarr
spec:
  app: Bazarr
```

- [ ] **Step 2: Create subgen.yaml**

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: subgen
spec:
  app: Subgen
```

- [ ] **Step 3: Add ports to APP_PORTS in smoke-test.sh**

Open `.github/smoke-test/smoke-test.sh` and add to the `APP_PORTS` associative array:

```bash
  [bazarr]=6767
  [subgen]=9000
```

- [ ] **Step 4: Commit**

```bash
git add .github/smoke-test/
git commit -m "feat: add Bazarr and Subgen smoke test fixtures"
```

---

## Task 13: Final build + test pass and clippy

- [ ] **Step 1: Full build**

```bash
cargo build --all-targets 2>&1 | tail -20
```

Expected: Compiles without errors or warnings.

- [ ] **Step 2: Full test suite**

```bash
cargo test 2>&1 | tail -30
```

Expected: All tests pass.

- [ ] **Step 3: Clippy**

```bash
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -30
```

Expected: No warnings. Fix any issues found before proceeding.

- [ ] **Step 4: Format**

```bash
cargo fmt --check 2>&1
```

Expected: No formatting differences. If there are, run `cargo fmt` and commit.

- [ ] **Step 5: Commit any remaining fixes**

```bash
git add -p
git commit -m "fix: address clippy and fmt issues"
```

---

## Self-Review Checklist

After writing this plan, verify against the spec:

- [x] `[bazarr]` and `[subgen]` TOML sections — Task 1
- [x] `AppType::Bazarr` with `as_str()="bazarr"`, `tier()=3` — Task 2
- [x] `AppType::Subgen` with `as_str()="subgen"`, `tier()=0` — Task 2
- [x] `BazarrSyncSpec` with `enabled`, `namespace_scope`, `auto_remove` — Task 3
- [x] `SubgenSyncSpec` with `enabled`, `namespace_scope` — Task 3
- [x] `bazarr_sync`, `subgen_sync` on `ServarrAppSpec` — Task 3
- [x] Subgen models PVC (`/subgen/models`, 10Gi) — Task 4
- [x] Subgen default env vars (`TRANSCRIBE_DEVICE=cpu`, `WHISPER_MODEL=medium`) — Task 4
- [x] `BazarrClient` with all methods — Task 5
- [x] Bazarr init ConfigMap (`bazarr-init.sh`) — Task 6
- [x] Bazarr init container in Deployment builder — Task 7
- [x] Operator-managed API key secret for Bazarr — Task 8
- [x] `sync_bazarr_apps` — Task 9
- [x] `sync_subgen_jellyfin` — Task 9
- [x] Bazarr in `sync_admin_credentials` (MD5 password) — Task 9
- [x] Bazarr and Subgen reconcile loop wiring — Tasks 9, 10
- [x] Helm artifacts regenerated — Task 11
- [x] Smoke test fixtures — Task 12
