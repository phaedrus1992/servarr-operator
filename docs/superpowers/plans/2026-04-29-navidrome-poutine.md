# Navidrome + Poutine App Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `Navidrome` (music server, tier 0) and `Poutine` (federated music hub, tier 3) as first-class `AppType` variants, including image defaults, env var injection, a `PoutineConfig` CRD field for federation peers (serialized to a mounted ConfigMap), and example YAMLs.

**Architecture:** Follow the exact pattern established by Subgen (env + PVC special-cases in `for_app`) and Prowlarr (AppConfig → ConfigMap → mounted volume). The only new surface area is `PoutineConfig`/`PoutinePeer` in `app_config.rs` and `build_poutine_peers` in `configmap.rs`. The `POUTINE_PEERS_CONFIG` env var is injected only when peers are configured.

**Tech Stack:** Rust (workspace), kube-rs CRDs, k8s-openapi, serde/schemars, serde_yaml, image-defaults.toml (build-time codegen via build.rs)

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `image-defaults.toml` | Modify | Add `[navidrome]` and `[poutine]` sections |
| `crates/servarr-crds/src/v1alpha1/spec.rs` | Modify | Add `Navidrome`, `Poutine` to `AppType` enum; `as_str()`, `tier()` |
| `crates/servarr-crds/src/v1alpha1/defaults.rs` | Modify | `validate_all()` array; `for_app()` special-cases for env + PVC |
| `crates/servarr-crds/src/v1alpha1/app_config.rs` | Modify | Add `PoutineConfig`, `PoutinePeer`; `Poutine(PoutineConfig)` to `AppConfig` enum |
| `crates/servarr-resources/src/configmap.rs` | Modify | Add `build_poutine_peers()`; wire into `build()` |
| `crates/servarr-resources/src/deployment.rs` | Modify | Add Poutine peers ConfigMap volume + VolumeMount |
| `crates/servarr-crds/tests/defaults_tests.rs` | Modify | Tests for Navidrome and Poutine defaults |
| `crates/servarr-resources/tests/builder_tests.rs` | Modify | Tests for Poutine peers ConfigMap and empty-peers case |
| `docs/examples/navidrome.yaml` | Create | Minimal Navidrome example with NFS music mount |
| `docs/examples/poutine.yaml` | Create | Poutine example with peers config |
| `README.md` | Modify | Add Navidrome and Poutine to Supported Applications table |
| `entities.json` | Modify | Add `"Navidrome"` and `"Poutine"` to `app_types` |

---

## Task 1: image-defaults.toml — add Navidrome and Poutine

**Files:**
- Modify: `image-defaults.toml`

- [ ] **Step 1: Add entries**

Append to the end of `image-defaults.toml`:

```toml
[navidrome]
repository = "deluan/navidrome"
tag = "0.61.2"
port = 4533
security = "nonroot"
downloads = false

[poutine]
repository = "ghcr.io/benders/poutine"
tag = "0.4.5"
port = 3000
security = "nonroot"
downloads = false
```

- [ ] **Step 2: Verify build still compiles**

```bash
cargo build -p servarr-crds 2>&1 | tail -5
```

Expected: compiles without error. The build.rs codegen will now include `navidrome` and `poutine` in the generated `image_defaults` match.

- [ ] **Step 3: Commit**

```bash
git add image-defaults.toml
git commit -m "feat(images): add Navidrome and Poutine image defaults"
```

---

## Task 2: AppType enum — add Navidrome and Poutine variants

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/spec.rs`

The `AppType` enum is in `spec.rs`. The `as_str()` and `tier()` methods are on the same enum impl block.

- [ ] **Step 1: Write failing tests**

In `crates/servarr-crds/tests/defaults_tests.rs`, add at the end of the file:

```rust
// ---------------------------------------------------------------------------
// Navidrome AppType
// ---------------------------------------------------------------------------

#[test]
fn navidrome_as_str() {
    assert_eq!(AppType::Navidrome.as_str(), "navidrome");
}

#[test]
fn navidrome_is_tier_zero() {
    assert_eq!(AppType::Navidrome.tier(), 0);
}

// ---------------------------------------------------------------------------
// Poutine AppType
// ---------------------------------------------------------------------------

#[test]
fn poutine_as_str() {
    assert_eq!(AppType::Poutine.as_str(), "poutine");
}

#[test]
fn poutine_is_tier_three() {
    assert_eq!(AppType::Poutine.tier(), 3);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p servarr-crds navidrome_as_str 2>&1 | tail -10
```

Expected: compile error — `AppType::Navidrome` not found.

- [ ] **Step 3: Add enum variants**

In `crates/servarr-crds/src/v1alpha1/spec.rs`, locate the `AppType` enum (currently ends with `Subgen`). Add two variants:

```rust
pub enum AppType {
    // ... existing variants ...
    Bazarr,
    Subgen,
    Navidrome,  // add
    Poutine,    // add
}
```

- [ ] **Step 4: Add `as_str()` arms**

In the `as_str()` match, add after `Self::Subgen => "subgen"`:

```rust
Self::Navidrome => "navidrome",
Self::Poutine => "poutine",
```

- [ ] **Step 5: Add `tier()` arms**

In the `tier()` match:

```rust
// Tier 0 arm — add Navidrome:
Self::Plex | Self::Jellyfin | Self::SshBastion | Self::Navidrome => 0,

// Tier 3 arm — add Poutine (after existing Subgen):
| Self::Subgen
| Self::Poutine => 3,
```

- [ ] **Step 6: Run tests to verify they pass**

```bash
cargo test -p servarr-crds navidrome_as_str navidrome_is_tier_zero poutine_as_str poutine_is_tier_three 2>&1 | tail -15
```

Expected: 4 tests pass.

- [ ] **Step 7: Run full crate tests**

```bash
cargo test -p servarr-crds 2>&1 | tail -20
```

Expected: all pass. (The `validate_all` exhaustive list will fail if you ran it — we'll add the variants there in Task 3.)

- [ ] **Step 8: Commit**

```bash
git add crates/servarr-crds/src/v1alpha1/spec.rs crates/servarr-crds/tests/defaults_tests.rs
git commit -m "feat(crds): add Navidrome and Poutine AppType variants"
```

---

## Task 3: AppDefaults — validate_all, for_app special cases

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/defaults.rs`

- [ ] **Step 1: Write failing tests**

In `crates/servarr-crds/tests/defaults_tests.rs`, add:

```rust
// ---------------------------------------------------------------------------
// Navidrome AppDefaults
// ---------------------------------------------------------------------------

#[test]
fn navidrome_defaults_port() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    assert_eq!(defaults.service.ports[0].port, 4533);
}

#[test]
fn navidrome_defaults_image() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    assert_eq!(defaults.image.repository, "deluan/navidrome");
    assert_eq!(defaults.image.tag, "0.61.2");
}

#[test]
fn navidrome_has_data_pvc() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has_data = defaults
        .persistence
        .volumes
        .iter()
        .any(|v| v.name == "data" && v.mount_path == "/data");
    assert!(has_data, "Navidrome should have a 'data' PVC at /data");
}

#[test]
fn navidrome_env_includes_nd_loglevel() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "ND_LOGLEVEL" && e.value == "info");
    assert!(has, "Navidrome should default ND_LOGLEVEL=info");
}

#[test]
fn navidrome_env_includes_nd_scanschedule() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "ND_SCANSCHEDULE" && e.value == "1h");
    assert!(has, "Navidrome should default ND_SCANSCHEDULE=1h");
}

#[test]
fn navidrome_env_includes_nd_sessiontimeout() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "ND_SESSIONTIMEOUT" && e.value == "24h");
    assert!(has, "Navidrome should default ND_SESSIONTIMEOUT=24h");
}

#[test]
fn navidrome_env_includes_nd_enableexternalservices() {
    let defaults = AppDefaults::for_app(&AppType::Navidrome);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "ND_ENABLEEXTERNALSERVICES" && e.value == "false");
    assert!(
        has,
        "Navidrome should default ND_ENABLEEXTERNALSERVICES=false"
    );
}

// ---------------------------------------------------------------------------
// Poutine AppDefaults
// ---------------------------------------------------------------------------

#[test]
fn poutine_defaults_port() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    assert_eq!(defaults.service.ports[0].port, 3000);
}

#[test]
fn poutine_defaults_image() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    assert_eq!(defaults.image.repository, "ghcr.io/benders/poutine");
    assert_eq!(defaults.image.tag, "0.4.5");
}

#[test]
fn poutine_has_data_pvc_at_app_data() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has_data = defaults
        .persistence
        .volumes
        .iter()
        .any(|v| v.name == "data" && v.mount_path == "/app/data");
    assert!(
        has_data,
        "Poutine should have a 'data' PVC at /app/data, got: {:?}",
        defaults.persistence.volumes
    );
}

#[test]
fn poutine_has_no_config_pvc() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has_config = defaults
        .persistence
        .volumes
        .iter()
        .any(|v| v.name == "config");
    assert!(
        !has_config,
        "Poutine should not have a 'config' PVC — config is managed by ConfigMap"
    );
}

#[test]
fn poutine_env_includes_node_env() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "NODE_ENV" && e.value == "production");
    assert!(has, "Poutine should default NODE_ENV=production");
}

#[test]
fn poutine_env_includes_database_path() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "DATABASE_PATH" && e.value == "/app/data/poutine.db");
    assert!(has, "Poutine should default DATABASE_PATH=/app/data/poutine.db");
}

#[test]
fn poutine_env_includes_private_key_path() {
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "POUTINE_PRIVATE_KEY_PATH" && e.value == "/app/data/poutine_ed25519.pem");
    assert!(
        has,
        "Poutine should default POUTINE_PRIVATE_KEY_PATH=/app/data/poutine_ed25519.pem"
    );
}

#[test]
fn poutine_no_peers_config_env_by_default() {
    // POUTINE_PEERS_CONFIG is injected dynamically only when peers are configured,
    // not as a compiled-in default env var.
    let defaults = AppDefaults::for_app(&AppType::Poutine);
    let has = defaults
        .env
        .iter()
        .any(|e| e.name == "POUTINE_PEERS_CONFIG");
    assert!(
        !has,
        "POUTINE_PEERS_CONFIG should not be a default env var (injected only when peers set)"
    );
}

#[test]
fn validate_all_includes_navidrome_and_poutine() {
    // This panics if either app is missing from image-defaults.toml or validate_all.
    AppDefaults::validate_all().expect("all app defaults should be valid");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p servarr-crds navidrome_defaults_port 2>&1 | tail -10
```

Expected: panic — `no image defaults for app: navidrome` (image-defaults.toml has the entry but `validate_all` doesn't list it yet; `for_app` will find it via codegen though — the actual failure here will be `validate_all_includes_navidrome_and_poutine` panicking because validate_all doesn't include them).

- [ ] **Step 3: Add Navidrome and Poutine to validate_all**

In `crates/servarr-crds/src/v1alpha1/defaults.rs`, the `validate_all` function contains an exhaustive array. Add `AppType::Navidrome` and `AppType::Poutine`:

```rust
let all = [
    AppType::Sonarr,
    AppType::Radarr,
    AppType::Lidarr,
    AppType::Prowlarr,
    AppType::Sabnzbd,
    AppType::Transmission,
    AppType::Tautulli,
    AppType::Overseerr,
    AppType::Maintainerr,
    AppType::Jackett,
    AppType::Jellyfin,
    AppType::Plex,
    AppType::SshBastion,
    AppType::Bazarr,
    AppType::Subgen,
    AppType::Navidrome,  // add
    AppType::Poutine,    // add
];
```

- [ ] **Step 4: Add Navidrome special-case in for_app**

The `nonroot_base` builder creates a single PVC named `"config"` at `"/config"`. Navidrome needs it renamed to `"data"` at `"/data"`. After the `defaults.image = image(...)` line in `for_app`, add:

```rust
if matches!(app, super::AppType::Navidrome) {
    // Rename the default "config" PVC to "data" at /data
    for vol in &mut defaults.persistence.volumes {
        if vol.name == "config" {
            vol.name = "data".into();
            vol.mount_path = "/data".into();
        }
    }
    defaults.env.extend([
        EnvVar { name: "ND_LOGLEVEL".into(),               value: "info".into() },
        EnvVar { name: "ND_SCANSCHEDULE".into(),           value: "1h".into() },
        EnvVar { name: "ND_SESSIONTIMEOUT".into(),         value: "24h".into() },
        EnvVar { name: "ND_ENABLEEXTERNALSERVICES".into(), value: "false".into() },
    ]);
}
```

- [ ] **Step 5: Add Poutine special-case in for_app**

After the Navidrome block, add:

```rust
if matches!(app, super::AppType::Poutine) {
    // Rename the default "config" PVC to "data" at /app/data
    for vol in &mut defaults.persistence.volumes {
        if vol.name == "config" {
            vol.name = "data".into();
            vol.mount_path = "/app/data".into();
        }
    }
    defaults.env.extend([
        EnvVar { name: "NODE_ENV".into(),                 value: "production".into() },
        EnvVar { name: "DATABASE_PATH".into(),            value: "/app/data/poutine.db".into() },
        EnvVar { name: "POUTINE_PRIVATE_KEY_PATH".into(), value: "/app/data/poutine_ed25519.pem".into() },
    ]);
}
```

Note: `POUTINE_PEERS_CONFIG` is intentionally omitted here. It is injected at deployment build time only when peers are configured (handled in Task 5, deployment.rs).

- [ ] **Step 6: Run the new defaults tests**

```bash
cargo test -p servarr-crds navidrome poutine validate_all 2>&1 | tail -20
```

Expected: all new tests pass.

- [ ] **Step 7: Run full crate test suite**

```bash
cargo test -p servarr-crds 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/servarr-crds/src/v1alpha1/defaults.rs crates/servarr-crds/tests/defaults_tests.rs
git commit -m "feat(crds): add Navidrome and Poutine AppDefaults with env and PVC overrides"
```

---

## Task 4: PoutineConfig and PoutinePeer in app_config.rs

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/app_config.rs`

- [ ] **Step 1: Write failing test**

In `crates/servarr-crds/tests/defaults_tests.rs`, add:

```rust
// ---------------------------------------------------------------------------
// PoutineConfig round-trip
// ---------------------------------------------------------------------------

#[test]
fn poutine_config_serializes_peers() {
    let config = PoutineConfig {
        peers: vec![
            PoutinePeer {
                id: "friend-instance".into(),
                url: "https://music.friend.example.com".into(),
                public_key: "ed25519:fooBARbaz==".into(),
            },
        ],
    };
    let json = serde_json::to_string(&config).expect("serialize");
    assert!(json.contains("friend-instance"));
    assert!(json.contains("ed25519:fooBARbaz=="));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p servarr-crds poutine_config_serializes_peers 2>&1 | tail -10
```

Expected: compile error — `PoutineConfig` not found.

- [ ] **Step 3: Add PoutineConfig, PoutinePeer, and AppConfig variant**

In `crates/servarr-crds/src/v1alpha1/app_config.rs`, add after the `// --- Overseerr ---` section at the bottom:

```rust
// --- Poutine ---

/// A federation peer for Poutine.
///
/// Each peer entry is written to `peers.yaml` and mounted at
/// `/app/config/peers.yaml` inside the container. All three fields are
/// required — Poutine will refuse to start if any are missing.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PoutinePeer {
    /// The peer's `POUTINE_INSTANCE_ID` value.
    pub id: String,
    /// Base URL of the peer hub (e.g. `"https://music.friend.example.com"`).
    pub url: String,
    /// ed25519 public key string shown on the peer's Settings page
    /// (e.g. `"ed25519:fooBARbaz=="`).
    pub public_key: String,
}

/// Poutine federation configuration.
///
/// When `peers` is non-empty the operator generates a `peers.yaml` ConfigMap
/// and mounts it read-only at `/app/config/peers.yaml`. When empty no
/// ConfigMap is created and Poutine runs in standalone mode.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PoutineConfig {
    /// List of federation peers.
    #[serde(default)]
    pub peers: Vec<PoutinePeer>,
}
```

Also add the new variant to the `AppConfig` enum at the top of the file:

```rust
pub enum AppConfig {
    Transmission(TransmissionConfig),
    Sabnzbd(SabnzbdConfig),
    Prowlarr(ProwlarrConfig),
    SshBastion(SshBastionConfig),
    Overseerr(Box<OverseerrConfig>),
    Poutine(PoutineConfig),  // add
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p servarr-crds poutine_config_serializes_peers 2>&1 | tail -10
```

Expected: PASS.

- [ ] **Step 5: Run full crate tests**

```bash
cargo test -p servarr-crds 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/servarr-crds/src/v1alpha1/app_config.rs crates/servarr-crds/tests/defaults_tests.rs
git commit -m "feat(crds): add PoutineConfig and PoutinePeer to AppConfig"
```

---

## Task 5: configmap.rs — build_poutine_peers

**Files:**
- Modify: `crates/servarr-resources/src/configmap.rs`

The Poutine peers ConfigMap contains a single key `peers.yaml` whose value is the YAML serialization of the peers list. Use `serde_yaml` (check `Cargo.toml` for the crate name — it may be `serde_yaml` or `serde-yaml`).

- [ ] **Step 1: Check serde_yaml availability**

```bash
grep -r 'serde.yaml\|serde_yaml' /Users/ranger/git/servarr-operator/crates/servarr-resources/Cargo.toml
```

If not present, check other crates:
```bash
grep -r 'serde.yaml' /Users/ranger/git/servarr-operator/Cargo.toml /Users/ranger/git/servarr-operator/crates/*/Cargo.toml
```

If `serde_yaml` is not in the workspace, add it to `servarr-resources/Cargo.toml` under `[dependencies]`:
```toml
serde_yaml = "0.9"
```

- [ ] **Step 2: Write failing tests**

In `crates/servarr-resources/tests/builder_tests.rs`, find the imports block at the top (it imports `servarr_crds::*`). Add the test at the end of the file:

```rust
#[test]
fn test_poutine_peers_configmap_with_peers() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("poutine".into()),
            namespace: Some("media".into()),
            uid: Some("uid-poutine-001".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Poutine,
            app_config: Some(AppConfig::Poutine(PoutineConfig {
                peers: vec![
                    PoutinePeer {
                        id: "friend-instance".into(),
                        url: "https://music.friend.example.com".into(),
                        public_key: "ed25519:fooBARbaz==".into(),
                    },
                    PoutinePeer {
                        id: "second-peer".into(),
                        url: "https://music.second.example.com".into(),
                        public_key: "ed25519:anotherKey==".into(),
                    },
                ],
            })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_poutine_peers(&app);
    assert!(
        cm.is_some(),
        "Poutine with peers should produce a ConfigMap"
    );
    let cm = cm.unwrap();
    let data = cm.data.unwrap();
    assert!(data.contains_key("peers.yaml"), "ConfigMap should have 'peers.yaml' key");
    let yaml = &data["peers.yaml"];
    assert!(yaml.contains("friend-instance"), "yaml should contain first peer id");
    assert!(yaml.contains("ed25519:fooBARbaz=="), "yaml should contain first peer public_key");
    assert!(yaml.contains("second-peer"), "yaml should contain second peer id");
}

#[test]
fn test_poutine_peers_configmap_empty_peers_returns_none() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("poutine".into()),
            namespace: Some("media".into()),
            uid: Some("uid-poutine-002".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Poutine,
            app_config: Some(AppConfig::Poutine(PoutineConfig { peers: vec![] })),
            ..Default::default()
        },
        status: None,
    };

    let cm = servarr_resources::configmap::build_poutine_peers(&app);
    assert!(
        cm.is_none(),
        "Poutine with empty peers should return None"
    );
}

#[test]
fn test_poutine_peers_configmap_no_app_config_returns_none() {
    let app = make_app(AppType::Poutine);
    let cm = servarr_resources::configmap::build_poutine_peers(&app);
    assert!(
        cm.is_none(),
        "Poutine with no app_config should return None"
    );
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test -p servarr-resources test_poutine_peers 2>&1 | tail -10
```

Expected: compile error — `build_poutine_peers` not found.

- [ ] **Step 4: Implement build_poutine_peers**

In `crates/servarr-resources/src/configmap.rs`, add at the end of the file (after `build_bazarr_init`). Also add `serde_yaml` to the imports at the top if not already present:

```rust
/// Build a ConfigMap containing the Poutine federation peers config.
///
/// Generates `peers.yaml` as a YAML list of peer objects and mounts it
/// at `/app/config/peers.yaml` inside the container. Returns `None` when
/// there are no peers configured (standalone mode).
pub fn build_poutine_peers(app: &ServarrApp) -> Option<ConfigMap> {
    let peers = match app.spec.app_config {
        Some(AppConfig::Poutine(ref pc)) if !pc.peers.is_empty() => &pc.peers,
        _ => return None,
    };

    // Serialize peers to YAML. serde_yaml serializes Vec<PoutinePeer> as a
    // YAML sequence, which is exactly the format Poutine expects.
    let yaml = serde_yaml::to_string(peers)
        .unwrap_or_else(|_| String::new());

    let mut data = BTreeMap::new();
    data.insert("peers.yaml".into(), yaml);

    Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(common::child_name(app, "poutine-peers")),
            namespace: Some(common::app_namespace(app)),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    })
}
```

Also wire it into `build()`:

```rust
pub fn build(app: &ServarrApp) -> Option<ConfigMap> {
    match app.spec.app {
        AppType::Transmission => build_transmission(app),
        AppType::Sabnzbd => build_sabnzbd(app),
        AppType::Poutine => build_poutine_peers(app),  // add
        _ => None,
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p servarr-resources test_poutine_peers 2>&1 | tail -15
```

Expected: 3 tests pass.

- [ ] **Step 6: Run full resources test suite**

```bash
cargo test -p servarr-resources 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/servarr-resources/src/configmap.rs crates/servarr-resources/tests/builder_tests.rs
git commit -m "feat(resources): add build_poutine_peers configmap builder"
```

---

## Task 6: deployment.rs — Poutine peers volume and VolumeMount

**Files:**
- Modify: `crates/servarr-resources/src/deployment.rs`

When Poutine has peers configured, two things need to happen:
1. A Volume backed by the `poutine-peers` ConfigMap is added to the pod spec.
2. A VolumeMount of `peers.yaml` (via `sub_path`) at `/app/config/peers.yaml` is added to the container.
3. The `POUTINE_PEERS_CONFIG` env var is added to the container env.

- [ ] **Step 1: Write failing test**

In `crates/servarr-resources/tests/builder_tests.rs`, add:

```rust
#[test]
fn test_poutine_peers_mounted_in_deployment() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("poutine".into()),
            namespace: Some("media".into()),
            uid: Some("uid-poutine-deploy".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Poutine,
            app_config: Some(AppConfig::Poutine(PoutineConfig {
                peers: vec![PoutinePeer {
                    id: "friend".into(),
                    url: "https://music.friend.example.com".into(),
                    public_key: "ed25519:key==".into(),
                }],
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    // Volume should be present
    let volumes = pod_spec.volumes.unwrap_or_default();
    let has_peers_vol = volumes.iter().any(|v| v.name == "poutine-peers");
    assert!(has_peers_vol, "Poutine deployment should have a 'poutine-peers' volume");

    // VolumeMount should be present with sub_path
    let container = &pod_spec.containers[0];
    let mounts = container.volume_mounts.as_ref().unwrap();
    let peers_mount = mounts.iter().find(|m| m.name == "poutine-peers");
    assert!(peers_mount.is_some(), "Poutine container should have a 'poutine-peers' VolumeMount");
    let peers_mount = peers_mount.unwrap();
    assert_eq!(peers_mount.mount_path, "/app/config/peers.yaml");
    assert_eq!(peers_mount.sub_path.as_deref(), Some("peers.yaml"));
    assert_eq!(peers_mount.read_only, Some(true));

    // POUTINE_PEERS_CONFIG env var should be present
    let env = container.env.as_ref().unwrap();
    let has_peers_env = env
        .iter()
        .any(|e| e.name == "POUTINE_PEERS_CONFIG" && e.value.as_deref() == Some("/app/config/peers.yaml"));
    assert!(has_peers_env, "Poutine with peers should have POUTINE_PEERS_CONFIG env var");
}

#[test]
fn test_poutine_no_peers_no_peers_volume() {
    let app = make_app(AppType::Poutine);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let volumes = pod_spec.volumes.unwrap_or_default();
    let has_peers_vol = volumes.iter().any(|v| v.name == "poutine-peers");
    assert!(
        !has_peers_vol,
        "Poutine without peers should not have a 'poutine-peers' volume"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p servarr-resources test_poutine_peers_mounted test_poutine_no_peers_no_peers_volume 2>&1 | tail -10
```

Expected: FAIL — volume and mount are not present yet.

- [ ] **Step 3: Add poutine-peers Volume in build_volumes**

In `crates/servarr-resources/src/deployment.rs`, inside `build_volumes`, add after the Prowlarr definitions block (around line 589):

```rust
// Poutine peers ConfigMap
if app
    .spec
    .app_config
    .as_ref()
    .is_some_and(|c| matches!(c, AppConfig::Poutine(pc) if !pc.peers.is_empty()))
{
    volumes.push(Volume {
        name: "poutine-peers".into(),
        config_map: Some(ConfigMapVolumeSource {
            name: common::child_name(app, "poutine-peers"),
            ..Default::default()
        }),
        ..Default::default()
    });
}
```

- [ ] **Step 4: Add poutine-peers VolumeMount in build_volume_mounts**

In `build_volume_mounts`, add after the Prowlarr definitions mount block (around line 405):

```rust
// Poutine peers config
if app
    .spec
    .app_config
    .as_ref()
    .is_some_and(|c| matches!(c, AppConfig::Poutine(pc) if !pc.peers.is_empty()))
{
    mounts.push(VolumeMount {
        name: "poutine-peers".into(),
        mount_path: "/app/config/peers.yaml".into(),
        sub_path: Some("peers.yaml".into()),
        read_only: Some(true),
        ..Default::default()
    });
}
```

- [ ] **Step 5: Inject POUTINE_PEERS_CONFIG env var in build_env_vars**

Find `build_env_vars` in `deployment.rs`. It builds env vars from defaults + CR overrides. Add a Poutine-specific injection before the merge loop. Locate the function and add near the end of the base env construction (after the app-type specific blocks, before the CR override merge):

```rust
// Poutine: inject POUTINE_PEERS_CONFIG when peers are configured
if app
    .spec
    .app_config
    .as_ref()
    .is_some_and(|c| matches!(c, AppConfig::Poutine(pc) if !pc.peers.is_empty()))
{
    base_env.push(EnvVar {
        name: "POUTINE_PEERS_CONFIG".into(),
        value: "/app/config/peers.yaml".into(),
        value_from: None,
    });
}
```

Note: `EnvVar` here is `k8s_openapi::api::core::v1::EnvVar` (already in scope in `deployment.rs`), not `servarr_crds::EnvVar`. Check the existing env var construction in `build_env_vars` to confirm the correct type and field names in use.

- [ ] **Step 6: Run tests to verify they pass**

```bash
cargo test -p servarr-resources test_poutine_peers_mounted test_poutine_no_peers_no_peers_volume 2>&1 | tail -15
```

Expected: 2 tests pass.

- [ ] **Step 7: Run full test suite**

```bash
cargo test -p servarr-resources 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/servarr-resources/src/deployment.rs crates/servarr-resources/tests/builder_tests.rs
git commit -m "feat(resources): mount Poutine peers ConfigMap and inject POUTINE_PEERS_CONFIG"
```

---

## Task 7: CRD generation check

**Files:**
- Read: `scripts/generate-crds.sh`
- Read: `crates/servarr-crds/build.rs`

The CRD YAML files are generated by the build script. Run it to ensure the new variants appear in the generated CRDs.

- [ ] **Step 1: Regenerate CRDs**

```bash
bash /Users/ranger/git/servarr-operator/scripts/generate-crds.sh
```

- [ ] **Step 2: Verify AppType in generated CRD**

```bash
grep -r 'Navidrome\|Poutine' /Users/ranger/git/servarr-operator/charts/ 2>/dev/null | head -20
```

Expected: `Navidrome` and `Poutine` appear in the CRD enum values.

- [ ] **Step 3: Commit generated CRD changes**

```bash
git add charts/
git commit -m "chore(crds): regenerate CRDs with Navidrome and Poutine variants"
```

---

## Task 8: Example YAMLs

**Files:**
- Create: `docs/examples/navidrome.yaml`
- Create: `docs/examples/poutine.yaml`

- [ ] **Step 1: Create navidrome.yaml**

```yaml
# Navidrome music server with NFS-mounted music library.
# Navidrome requires credentials set at first boot via the web UI.
# Connect Poutine to this instance via NAVIDROME_URL, NAVIDROME_USERNAME,
# and NAVIDROME_PASSWORD env vars on the Poutine ServarrApp.
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: navidrome
  namespace: media
spec:
  app: Navidrome
  persistence:
    nfsMounts:
      - name: music
        server: nas.local
        path: /music
        mountPath: /music
        readOnly: true
```

- [ ] **Step 2: Create poutine.yaml**

```yaml
# Poutine federated music hub.
# Required env vars (set via a Secret or directly):
#   NAVIDROME_URL       — internal URL of the Navidrome service
#   NAVIDROME_USERNAME  — Navidrome admin username
#   NAVIDROME_PASSWORD  — Navidrome admin password
#   POUTINE_INSTANCE_ID — unique ID for this Poutine instance
#   POUTINE_OWNER_USERNAME / POUTINE_OWNER_PASSWORD — initial owner account
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: poutine
  namespace: media
spec:
  app: Poutine
  env:
    - name: NAVIDROME_URL
      value: "http://navidrome:4533"
    - name: POUTINE_INSTANCE_ID
      value: "my-poutine-instance"
  appConfig:
    poutine:
      peers:
        - id: "friend-instance"
          url: "https://music.friend.example.com"
          publicKey: "ed25519:fooBARbaz=="
```

- [ ] **Step 3: Commit**

```bash
git add docs/examples/navidrome.yaml docs/examples/poutine.yaml
git commit -m "docs: add Navidrome and Poutine example ServarrApp manifests"
```

---

## Task 9: README and entities.json

**Files:**
- Modify: `README.md`
- Modify: `entities.json`

- [ ] **Step 1: Update README supported apps table**

In `README.md`, find the Supported Applications table and add two rows:

```markdown
| Navidrome | Music server | 4533 | 0 - Media Servers |
| Poutine | Federated music hub | 3000 | 3 - Ancillary |
```

Place Navidrome after Jellyfin (both tier 0). Place Poutine after Maintainerr (tier 3).

- [ ] **Step 2: Update entities.json**

In `entities.json`, add `"Navidrome"` and `"Poutine"` to the `app_types` array:

```json
"app_types": [
  "Sonarr", "Radarr", "Lidarr", "Prowlarr", "Sabnzbd", "Transmission",
  "Tautulli", "Overseerr", "Maintainerr", "Jackett", "Jellyfin", "Plex",
  "SshBastion", "Bazarr", "Subgen", "Navidrome", "Poutine"
]
```

- [ ] **Step 3: Commit**

```bash
git add README.md entities.json
git commit -m "docs: add Navidrome and Poutine to supported apps"
```

---

## Task 10: Full build and lint

- [ ] **Step 1: Clippy — zero warnings**

```bash
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -20
```

Expected: no warnings or errors. Fix any that appear before proceeding.

- [ ] **Step 2: Full test suite**

```bash
cargo test 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 3: Commit any lint fixes**

If Step 1 required fixes:
```bash
git add -p
git commit -m "fix(lint): clippy fixes for Navidrome and Poutine support"
```

---

## Implementation Notes

**serde_yaml and EnvVar type:** Before Task 5, check whether `serde_yaml` is already a dependency via:
```bash
grep -r 'serde.yaml' /Users/ranger/git/servarr-operator/Cargo.toml /Users/ranger/git/servarr-operator/crates/*/Cargo.toml
```
If absent, add to `crates/servarr-resources/Cargo.toml`. The `serde_yaml::to_string` call serializes the `Vec<PoutinePeer>` as a YAML sequence.

**EnvVar in deployment.rs:** The `deployment.rs` file uses `k8s_openapi::api::core::v1::EnvVar` (aliased at the import block), not `servarr_crds::EnvVar`. Check the existing env var construction pattern in `build_env_vars` (search for `EnvVar {`) to confirm the struct fields in use (`name`, `value`, `value_from`).

**build_volume_mounts location:** The function is earlier in `deployment.rs` than `build_volumes`. Both need Poutine blocks added.
