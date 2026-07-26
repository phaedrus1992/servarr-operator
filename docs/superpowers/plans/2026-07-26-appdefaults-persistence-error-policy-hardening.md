# AppDefaults Persistence & Error-Policy Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close three follow-up gaps left open by the #367/#368 `AppDefaults` PR: let admins intentionally remove a compiled default persistence volume, fail loudly on a mount-path collision instead of producing an invalid pod spec, stop leaking internal `AppDefaults` error text into tenant-visible Kubernetes Events, and delete the now-redundant `AppDefaults::for_app` alias.

**Architecture:** All work is in the existing `AppDefaults`/`PersistenceSpec` types (`crates/servarr-crds/src/v1alpha1/`) and the `Error`/`error_policy` pair in `crates/servarr-operator/src/controller.rs`. No new files, no new crates.

**Tech Stack:** Rust workspace (`servarr-crds`, `servarr-resources`, `servarr-operator`), `serde`/`schemars` for CRD types, `thiserror` for the operator `Error` enum, `wiremock` + `tokio::test` for controller-level tests, `proptest` for CRD-type round-trip tests.

## Global Constraints

- Never edit `Cargo.toml`/`Cargo.lock` version fields (managed by `cargo-release`).
- Base branch is `release/1.x`; commit messages use `Fixes #378` / `Fixes #376` / `Fixes #377` (auto-close on merge) plus `Fixes #386` on the task that closes out the composite (put all four in the final task's commit body, or spread `refs #386` through earlier commits and the final `Fixes #386` on the last one — either is fine, just make sure all four appear across the branch's commits before the PR body is written).
- Every new/changed field on a CRD type is camelCase over the wire — `PersistenceSpec` already carries `#[serde(rename_all = "camelCase")]`, so `removed_default_volumes` in Rust becomes `removedDefaultVolumes` in YAML automatically; do not add a manual `#[serde(rename = ...)]`.
- `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check`, and `cargo test --workspace` must pass before every commit (CI runs Rust 1.94, stricter than some local toolchains — run clippy locally before pushing).

---

### Task 1: Delete `AppDefaults::for_app`, repoint all call sites to `try_for_app` (#378)

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/defaults.rs:126-128` (delete `for_app`)
- Modify: every call site matched by `rg -l '\bfor_app\('` across `crates/servarr-resources/src/`, `crates/servarr-resources/tests/`, `crates/servarr-crds/tests/`, `crates/servarr-operator/src/` (64 occurrences of `for_app(` total per current `rg -c` count; `try_for_app` itself and its own definition are excluded from that count since they don't match the bare `for_app(` pattern... but double check: `try_for_app(` also contains the substring `for_app(`, so the raw count needs the case-sensitive whole-call check below)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing new — this is a pure rename, `try_for_app`'s signature (`pub fn try_for_app(app: &super::AppType) -> Result<Self, String>`) is unchanged and is what every call site ends up calling.

This is a zero-behavior-change mechanical rename (the two functions have been identical since #305 removed the panic that used to distinguish them) — there is no new behavior to drive with a failing test. The existing test suite (which already exercises `AppDefaults::for_app`/`try_for_app` extensively) is the regression net; the "test" for this task is that the full workspace still builds and passes after every call site is repointed and the alias is deleted.

- [ ] **Step 1: Enumerate the real call sites**

`for_app(` as a raw string also matches inside `try_for_app(`. Get the precise list of lines that call the alias specifically (word boundary before `for_app`, not preceded by `try_`):

```bash
rg -n '(?<!try_)\bfor_app\(' crates -g '*.rs'
```

Confirm the only definition-site hit is `crates/servarr-crds/src/v1alpha1/defaults.rs:126:    pub fn for_app(app: &super::AppType) -> Result<Self, String> {` — every other hit is a call site to fix.

- [ ] **Step 2: Repoint every call site**

For each file `rg` reported, replace `AppDefaults::for_app(` with `AppDefaults::try_for_app(` (some call sites may write it as `Self::for_app(` inside `impl AppDefaults` blocks, or via a `use` alias — check the actual text at each hit rather than assuming the fully-qualified form). A safe bulk approach, since `try_for_app` is never itself called as `for_app` anywhere apart from the alias's own body:

```bash
rg -l '(?<!try_)\bfor_app\(' crates -g '*.rs' | xargs sed -i '' -E 's/([A-Za-z_]*::)?\bfor_app\(/\1try_for_app(/g'
```

(macOS `sed -i ''`; the capture group preserves any `AppDefaults::`/`Self::` prefix.) Re-run the Step 1 `rg` — it must now return only the `defaults.rs:126` definition line.

- [ ] **Step 3: Delete the alias**

Remove from `crates/servarr-crds/src/v1alpha1/defaults.rs`:

```rust
    pub fn for_app(app: &super::AppType) -> Result<Self, String> {
        Self::try_for_app(app)
    }
```

Also trim the now-stale doc comment on `try_for_app` (lines 20-27) that describes `for_app` as an alias — update it to describe `try_for_app` on its own terms:

```rust
    /// Load defaults for `app`, returning an error if the app has no entry in
    /// `image-defaults.toml` or its security profile is unrecognised.
    ///
    /// Propagates the error to the caller rather than panicking. Call
    /// [`validate_all`] at startup to catch a broken `image-defaults.toml`
    /// before the first reconcile.
    ///
    /// # Errors
    ///
    /// Returns an error string if the app has no image defaults or an unknown
    /// security profile.
```

- [ ] **Step 4: Full workspace build + test**

```bash
cargo build --workspace --all-targets
cargo test --workspace
```

Expected: both succeed with zero errors. If `cargo build` reports an unresolved `for_app` reference, Step 2's substitution missed a call site — fix it and re-run.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
refactor: delete redundant AppDefaults::for_app alias

Fixes #378
EOF
)"
```

---

### Task 2: Add `removed_default_volumes` tombstone field to `PersistenceSpec` (#376)

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/types.rs:86-122` (`PersistenceSpec` struct + `merge_with`)
- Modify: every `PersistenceSpec { .. }` struct literal that cargo reports as missing the new field (see Step 3) — expected in `crates/servarr-operator/src/webhook.rs`, `crates/servarr-crds/src/v1alpha1/defaults.rs`, `crates/servarr-crds/src/v1alpha1/media_stack.rs`, `crates/servarr-crds/tests/{crd_tests,defaults_tests,media_stack_tests,proptest_crd_types}.rs`, `crates/servarr-resources/tests/builder_tests.rs`
- Test: `crates/servarr-crds/tests/defaults_tests.rs` (new test appended near the existing `resolve_persistence_*` tests)

**Interfaces:**
- Produces: `PersistenceSpec.removed_default_volumes: Vec<String>` (camelCase `removedDefaultVolumes` on the wire), and `PersistenceSpec::merge_with` now carries it through (`self.removed_default_volumes` wins — the override is always `self` per the existing "receiver wins" convention).

- [ ] **Step 1: Write the failing test**

Append to `crates/servarr-crds/tests/defaults_tests.rs`:

```rust
/// `PersistenceSpec::merge_with` treats `self` (the override) as the source
/// of truth for `removed_default_volumes` — the compiled-defaults side never
/// populates it, so there's nothing to fall back to.
#[test]
fn persistence_merge_with_carries_removed_default_volumes() {
    let override_spec = PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![],
        removed_default_volumes: vec!["downloads".into()],
    };
    let base = PersistenceSpec {
        volumes: vec![PvcVolume {
            name: "downloads".into(),
            mount_path: "/downloads".into(),
            access_mode: "ReadWriteOnce".into(),
            size: "100Gi".into(),
            storage_class: String::new(),
            existing_claim_name: None,
        }],
        nfs_mounts: vec![],
        removed_default_volumes: vec![],
    };

    let merged = override_spec.merge_with(&base);

    assert_eq!(merged.removed_default_volumes, vec!["downloads".to_string()]);
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
cargo test -p servarr-crds --test defaults_tests persistence_merge_with_carries_removed_default_volumes
```

Expected: compile error — `PersistenceSpec` has no field `removed_default_volumes` yet.

- [ ] **Step 3: Add the field and thread it through `merge_with`**

In `crates/servarr-crds/src/v1alpha1/types.rs`, change:

```rust
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersistenceSpec {
    #[serde(default)]
    pub volumes: Vec<PvcVolume>,
    #[serde(default)]
    pub nfs_mounts: Vec<NfsMount>,
}
```

to:

```rust
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersistenceSpec {
    #[serde(default)]
    pub volumes: Vec<PvcVolume>,
    #[serde(default)]
    pub nfs_mounts: Vec<NfsMount>,
    /// Names of compiled default volumes (see
    /// `AppDefaults::resolve_persistence`) this override intentionally
    /// removes rather than replaces. Without this, `resolve_persistence`
    /// always restores any default volume a whole-list-replace override
    /// drops (#367), so there was previously no way to say "I mean to drop
    /// this one" (#376). An explicit override that re-lists the same name in
    /// `volumes` still wins over the tombstone.
    #[serde(default)]
    pub removed_default_volumes: Vec<String>,
}
```

and change `merge_with`:

```rust
    pub fn merge_with(&self, base: &PersistenceSpec) -> PersistenceSpec {
        let volumes = if self.volumes.is_empty() {
            base.volumes.clone()
        } else {
            self.volumes.clone()
        };

        let mut nfs_map: IndexMap<String, NfsMount> = IndexMap::new();
        for m in &base.nfs_mounts {
            nfs_map.insert(m.name.clone(), m.clone());
        }
        for m in &self.nfs_mounts {
            nfs_map.insert(m.name.clone(), m.clone());
        }

        PersistenceSpec {
            volumes,
            nfs_mounts: nfs_map.into_values().collect(),
            removed_default_volumes: self.removed_default_volumes.clone(),
        }
    }
```

(only the trailing field is new; `volumes`/`nfs_mounts` construction is unchanged.)

- [ ] **Step 4: Fix compile fallout at every other `PersistenceSpec { .. }` literal**

```bash
cargo build --workspace --all-targets 2>&1 | rg -B2 "missing field \`removed_default_volumes\`"
```

For each reported literal, append `..Default::default()` as the literal's last line (functional update syntax — fills in only the fields not already set, i.e. just the new one). Example (`crates/servarr-crds/tests/crd_tests.rs:90-106`):

```rust
        persistence: Some(PersistenceSpec {
            volumes: vec![PvcVolume { /* ... unchanged ... */ }],
            nfs_mounts: vec![NfsMount { /* ... unchanged ... */ }],
            ..Default::default()
        }),
```

Also update the proptest generator so property tests cover the new field — in `crates/servarr-crds/tests/proptest_crd_types.rs`:

```rust
fn arb_persistence() -> impl Strategy<Value = PersistenceSpec> {
    (
        prop::collection::vec(arb_pvc(), 0..4),
        prop::collection::vec(arb_nfs_mount(), 0..4),
        prop::collection::vec(arb_string(), 0..3),
    )
        .prop_map(|(volumes, nfs_mounts, removed_default_volumes)| PersistenceSpec {
            volumes,
            nfs_mounts,
            removed_default_volumes,
        })
}
```

Re-run the `cargo build` filter after each fix; repeat until it reports nothing.

- [ ] **Step 5: Run the test to confirm it passes**

```bash
cargo test -p servarr-crds --test defaults_tests persistence_merge_with_carries_removed_default_volumes
cargo test --workspace
```

Expected: PASS, full workspace still green.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat: add removedDefaultVolumes tombstone field to PersistenceSpec

refs #376
EOF
)"
```

---

### Task 3: `resolve_persistence` honors the tombstone list (#376)

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/defaults.rs:143-158` (`resolve_persistence`)
- Test: `crates/servarr-crds/tests/defaults_tests.rs`

**Interfaces:**
- Consumes: `PersistenceSpec.removed_default_volumes` from Task 2.
- Produces: `resolve_persistence` still returns plain `PersistenceSpec` in this task (the `Result` signature change is Task 4, kept separate so this task's diff is reviewable on its own).

- [ ] **Step 1: Write the failing test**

Append to `crates/servarr-crds/tests/defaults_tests.rs`:

```rust
/// An admin who explicitly tombstones a default volume via
/// `removedDefaultVolumes` must have it actually removed — not silently
/// restored by the very safety net #367 added (#376).
#[test]
fn resolve_persistence_honors_removed_default_volumes_tombstone() {
    let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
    let mut app = make_app(AppType::Sonarr);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![],
        removed_default_volumes: vec!["downloads".into()],
    });

    let persistence = defaults.resolve_persistence(&app);

    assert!(
        !persistence.volumes.iter().any(|v| v.name == "downloads"),
        "tombstoned default volume must not be restored"
    );
    assert!(
        persistence.volumes.iter().any(|v| v.name == "config"),
        "non-tombstoned default volumes must still be restored"
    );
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
cargo test -p servarr-crds --test defaults_tests resolve_persistence_honors_removed_default_volumes_tombstone
```

Expected: FAIL — `downloads` is still present (today's `merge_with` falls back to `base.volumes` wholesale when the override's `volumes` list is empty, which includes `downloads`; nothing currently removes it).

- [ ] **Step 3: Implement**

Replace `resolve_persistence` in `crates/servarr-crds/src/v1alpha1/defaults.rs`:

```rust
    pub fn resolve_persistence(&self, app: &super::ServarrApp) -> PersistenceSpec {
        let override_spec = app.spec.persistence.as_ref();

        let mut persistence = match override_spec {
            None => self.persistence.clone(),
            Some(spec) => spec.merge_with(&self.persistence),
        };

        // A tombstoned name is dropped unless the override itself re-lists
        // that volume explicitly — explicit still wins over "remove this".
        let tombstoned = override_spec
            .map(|spec| spec.removed_default_volumes.as_slice())
            .unwrap_or(&[]);
        let explicitly_kept: std::collections::HashSet<&str> = override_spec
            .map(|spec| spec.volumes.iter().map(|v| v.name.as_str()).collect())
            .unwrap_or_default();
        let is_removed =
            |name: &str| tombstoned.iter().any(|n| n == name) && !explicitly_kept.contains(name);

        for default_vol in &self.persistence.volumes {
            if is_removed(&default_vol.name) {
                continue;
            }
            if !persistence
                .volumes
                .iter()
                .any(|v| v.name == default_vol.name)
            {
                persistence.volumes.push(default_vol.clone());
            }
        }
        persistence.volumes.retain(|v| !is_removed(&v.name));

        persistence
    }
```

The trailing `retain` is what makes the tombstone work even when the override's `volumes` list is empty (the `merge_with` fallback path that inherits all of `base.volumes`, `downloads` included) — the `continue` inside the loop only covers the case where the override's non-empty `volumes` already dropped the default and the old restore-loop would otherwise re-add it.

- [ ] **Step 4: Run the test to confirm it passes**

```bash
cargo test -p servarr-crds --test defaults_tests
```

Expected: `resolve_persistence_honors_removed_default_volumes_tombstone` PASSes and every pre-existing `resolve_persistence_*` test in the same file still PASSes (they all pass `removed_default_volumes: vec![]` implicitly via `..Default::default()` from Task 2, so `is_removed` is always `false` for them — no behavior change).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
fix: resolve_persistence honors removedDefaultVolumes tombstone

refs #376
EOF
)"
```

---

### Task 4: `resolve_persistence` fails loudly on a mount-path collision (#376)

**Files:**
- Modify: `crates/servarr-crds/src/v1alpha1/defaults.rs` (`resolve_persistence` signature + new `find_mount_path_collision` helper)
- Modify: `crates/servarr-resources/src/deployment.rs:95`, `crates/servarr-resources/src/pvc.rs:13` (propagate the new `Result`)
- Modify: `crates/servarr-crds/tests/defaults_tests.rs` (all existing `resolve_persistence` call sites need `.unwrap()`)
- Test: `crates/servarr-crds/tests/defaults_tests.rs` (two new tests)

**Interfaces:**
- Consumes: Task 3's `resolve_persistence` body.
- Produces: `pub fn resolve_persistence(&self, app: &super::ServarrApp) -> Result<PersistenceSpec, String>` — every caller must now handle the `Result`. `deployment.rs::build` and `pvc.rs::build_all` already return `Result<_, String>` and already `?`-propagate `AppDefaults` errors via `common::log_app_defaults_error`, so this is a drop-in `?` at both call sites.

- [ ] **Step 1: Write the failing tests**

Append to `crates/servarr-crds/tests/defaults_tests.rs`:

```rust
/// Two persistence entries claiming the same `mount_path` produce an invalid
/// pod spec downstream (two `volumeMounts` at one path) — this must fail the
/// reconcile loudly instead of silently reaching the API server (#376).
#[test]
fn resolve_persistence_errors_on_mount_path_collision() {
    let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
    let mut app = make_app(AppType::Sonarr);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![NfsMount {
            name: "downloads-nfs".into(),
            server: "nas.local".into(),
            path: "/export/downloads".into(),
            mount_path: "/downloads".into(),
            read_only: false,
        }],
        removed_default_volumes: vec![],
    });

    let err = defaults.resolve_persistence(&app).expect_err(
        "an NFS mount colliding with the still-restored 'downloads' default PVC must fail loudly",
    );

    assert!(
        err.contains("/downloads"),
        "error should name the colliding mount_path, got: {err}"
    );
}

/// Tombstoning the colliding default volume (rather than leaving it to
/// collide) is exactly how an admin is meant to resolve this (#376) — it
/// must not also trip the collision check.
#[test]
fn resolve_persistence_removed_default_volume_allows_nfs_mount_at_same_path() {
    let defaults = AppDefaults::try_for_app(&AppType::Sonarr).unwrap();
    let mut app = make_app(AppType::Sonarr);
    app.spec.persistence = Some(PersistenceSpec {
        volumes: vec![],
        nfs_mounts: vec![NfsMount {
            name: "downloads-nfs".into(),
            server: "nas.local".into(),
            path: "/export/downloads".into(),
            mount_path: "/downloads".into(),
            read_only: false,
        }],
        removed_default_volumes: vec!["downloads".into()],
    });

    let persistence = defaults
        .resolve_persistence(&app)
        .expect("tombstoning the colliding default volume must let the override's NFS mount through");

    assert!(
        !persistence.volumes.iter().any(|v| v.name == "downloads"),
        "tombstoned default volume must not be restored"
    );
    assert!(persistence.nfs_mounts.iter().any(|m| m.mount_path == "/downloads"));
    assert!(
        persistence.volumes.iter().any(|v| v.name == "config"),
        "other default volumes must still be restored"
    );
}
```

- [ ] **Step 2: Run them to confirm they fail**

```bash
cargo test -p servarr-crds --test defaults_tests resolve_persistence_errors_on_mount_path_collision resolve_persistence_removed_default_volume_allows_nfs_mount_at_same_path
```

Expected: compile errors (`resolve_persistence` doesn't return a `Result` yet, so `.expect_err`/`.expect` don't type-check).

- [ ] **Step 3: Implement the collision check and the `Result` signature**

In `crates/servarr-crds/src/v1alpha1/defaults.rs`, change the `resolve_persistence` signature and ending (body from Task 3 is otherwise unchanged up through the `retain` line):

```rust
    pub fn resolve_persistence(&self, app: &super::ServarrApp) -> Result<PersistenceSpec, String> {
        let override_spec = app.spec.persistence.as_ref();

        let mut persistence = match override_spec {
            None => self.persistence.clone(),
            Some(spec) => spec.merge_with(&self.persistence),
        };

        let tombstoned = override_spec
            .map(|spec| spec.removed_default_volumes.as_slice())
            .unwrap_or(&[]);
        let explicitly_kept: std::collections::HashSet<&str> = override_spec
            .map(|spec| spec.volumes.iter().map(|v| v.name.as_str()).collect())
            .unwrap_or_default();
        let is_removed =
            |name: &str| tombstoned.iter().any(|n| n == name) && !explicitly_kept.contains(name);

        for default_vol in &self.persistence.volumes {
            if is_removed(&default_vol.name) {
                continue;
            }
            if !persistence
                .volumes
                .iter()
                .any(|v| v.name == default_vol.name)
            {
                persistence.volumes.push(default_vol.clone());
            }
        }
        persistence.volumes.retain(|v| !is_removed(&v.name));

        if let Some(msg) = find_mount_path_collision(&persistence.volumes, &persistence.nfs_mounts)
        {
            return Err(msg);
        }

        Ok(persistence)
    }
```

Add the helper as a free function near the bottom of the file, alongside `image`/`pvc`:

```rust
/// Kubernetes rejects a pod spec with two `volumeMounts` at the same path —
/// this catches that at resolve time (across both PVC volumes and NFS
/// mounts) so the reconcile fails loudly with a clear cause instead of
/// producing an invalid pod spec the API server silently rejects (#376).
fn find_mount_path_collision(volumes: &[PvcVolume], nfs_mounts: &[NfsMount]) -> Option<String> {
    let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for v in volumes {
        if let Some(prior) = seen.insert(v.mount_path.as_str(), v.name.as_str()) {
            return Some(format!(
                "persistence volumes '{prior}' and '{}' both mount at '{}'",
                v.name, v.mount_path
            ));
        }
    }
    for m in nfs_mounts {
        if let Some(prior) = seen.insert(m.mount_path.as_str(), m.name.as_str()) {
            return Some(format!(
                "persistence entries '{prior}' and '{}' both mount at '{}'",
                m.name, m.mount_path
            ));
        }
    }
    None
}
```

- [ ] **Step 4: Propagate the `Result` at both production call sites**

`crates/servarr-resources/src/deployment.rs:95`, change:

```rust
    let persistence = defaults.resolve_persistence(app);
```

to:

```rust
    let persistence = defaults
        .resolve_persistence(app)
        .inspect_err(|e| common::log_app_defaults_error(app, e))?;
```

`crates/servarr-resources/src/pvc.rs:13`, same change:

```rust
    let persistence = defaults
        .resolve_persistence(app)
        .inspect_err(|e| common::log_app_defaults_error(app, e))?;
```

(Both functions already return `Result<_, String>` and already use this exact `.inspect_err(...)?` pattern one line above for `AppDefaults::try_for_app` — this is the same established convention, not a new one.)

- [ ] **Step 5: Fix every remaining `resolve_persistence` call site**

```bash
cargo build --workspace --all-targets 2>&1 | rg "resolve_persistence|mismatched types" -A3
```

Every call in `crates/servarr-crds/tests/defaults_tests.rs` (the 6 pre-existing tests plus the 2 new ones from Task 3 — 8 total, all of which want the `Ok` value, none of which expect an error) changes from:

```rust
    let persistence = defaults.resolve_persistence(&app);
```

to:

```rust
    let persistence = defaults.resolve_persistence(&app).unwrap();
```

The two new tests from this task already call `.expect_err(...)` / `.expect(...)` directly and need no further change.

- [ ] **Step 6: Run the tests to confirm they pass**

```bash
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all green, no clippy warnings (watch for `map_or`/closure-related lints on the `is_removed` closure and the `HashMap`/`HashSet` usage — fix inline if clippy flags anything, e.g. it may prefer `.any(|n| n == name)` over an eq-by-ref variant, or suggest `HashSet::from_iter`).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
fix: resolve_persistence fails loudly on mount_path collision

Fixes #376
EOF
)"
```

---

### Task 5: Document `removedDefaultVolumes` and regenerate CRD manifests (#376)

**Files:**
- Modify: `docs/configuration.md:291-340` (`persistence` section)
- Generate: `charts/servarr-crds/templates/servarrapp-crd.yaml`, `charts/servarr-crds/templates/mediastack-crd.yaml`

**Interfaces:** none (docs + generated-artifact task, no new code).

- [ ] **Step 1: Update the persistence docs**

In `docs/configuration.md`, change the sub-field table (currently lines 297-301):

```markdown
| Sub-field | Type | Default |
|---|---|---|
| `volumes` | `[]PvcVolume` | Per-app defaults |
| `nfsMounts` | `[]NfsMount` | `[]` |
```

to:

```markdown
| Sub-field | Type | Default |
|---|---|---|
| `volumes` | `[]PvcVolume` | Per-app defaults |
| `nfsMounts` | `[]NfsMount` | `[]` |
| `removedDefaultVolumes` | `[]string` | `[]` |
```

and immediately after the existing `**NfsMount fields:**` table + its explanatory paragraph (before the `spec: persistence:` YAML example, i.e. after current line 324), insert:

```markdown
**`removedDefaultVolumes`:** names of compiled default volumes (e.g. `downloads`, `config`)
this app-type would normally get automatically that the override intentionally removes, rather
than replaces. Without naming a volume here, the operator always restores any app-type default
volume a `volumes` override drops -- there is no other way to opt out of a default volume. Listing
the same name in `volumes` still wins over a tombstone here.
```

Then extend the existing YAML example (current lines 326-340) to show it in use:

```yaml
spec:
  persistence:
    volumes:
      - name: config
        mountPath: /config
        size: 5Gi
        storageClass: longhorn
    nfsMounts:
      - name: media
        server: nas.local
        path: /volume1/media
        mountPath: /media
        readOnly: false
      # Replace the app-type's default `downloads` PVC with this NFS mount at
      # the same path -- requires removedDefaultVolumes below, otherwise the
      # operator restores the dropped `downloads` PVC and the reconcile fails
      # on the mount_path collision.
      - name: downloads-nfs
        server: nas.local
        path: /volume1/downloads
        mountPath: /downloads
        readOnly: false
    removedDefaultVolumes:
      - downloads
```

- [ ] **Step 2: Regenerate the CRD manifests**

```bash
bash scripts/generate-crds.sh
git diff --stat charts/servarr-crds/templates/
```

Expected: both `servarrapp-crd.yaml` and `mediastack-crd.yaml` (persistence is used by both `ServarrApp` and `MediaStack`) show a diff adding a `removedDefaultVolumes` property under every `persistence` schema block.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
docs: document removedDefaultVolumes and regenerate CRD manifests

refs #376
EOF
)"
```

---

### Task 6: `Error::tenant_message` — decide what's safe to publish to tenant Events (#377)

**Files:**
- Modify: `crates/servarr-operator/src/controller.rs:39-47` (`Error` enum — add an `impl Error` block)
- Test: `crates/servarr-operator/src/controller.rs` `#[cfg(test)] mod tests` (near the existing `error_display_*` tests around line 3611)

**Interfaces:**
- Produces: `impl Error { pub fn tenant_message(&self) -> String }` — used by Task 7's `error_policy`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/servarr-operator/src/controller.rs`, near the existing `error_display_kube_variant`/`error_display_serialization_variant` tests:

```rust
    // ---- Error::tenant_message ----

    #[test]
    fn tenant_message_app_defaults_is_generic_not_raw_display() {
        let err = Error::AppDefaults(
            "unknown security profile in image-defaults.toml: bogus".to_string(),
        );
        let tenant_msg = err.tenant_message();

        assert!(
            !tenant_msg.contains("image-defaults.toml"),
            "tenant-visible message must not leak internal config file details, got: {tenant_msg}"
        );
        assert_ne!(
            tenant_msg,
            err.to_string(),
            "AppDefaults tenant_message must differ from the raw Display text"
        );
    }

    #[test]
    fn tenant_message_kube_variant_matches_display() {
        let invalid_bytes = vec![0xff, 0xfe];
        let utf8_err = String::from_utf8(invalid_bytes).unwrap_err();
        let err = Error::Kube(kube::Error::FromUtf8(utf8_err));

        assert_eq!(err.tenant_message(), err.to_string());
    }

    #[test]
    fn tenant_message_serialization_variant_matches_display() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err = Error::Serialization(json_err);

        assert_eq!(err.tenant_message(), err.to_string());
    }
```

- [ ] **Step 2: Run them to confirm they fail**

```bash
cargo test -p servarr-operator --lib tenant_message
```

Expected: compile error — `Error` has no method `tenant_message` yet.

- [ ] **Step 3: Implement**

Add, directly below the `Error` enum definition in `crates/servarr-operator/src/controller.rs`:

```rust
impl Error {
    /// Message safe to publish on a tenant-visible Kubernetes `Event` (any
    /// namespace-scoped Event-read RBAC holder can see it, a broader
    /// audience than controller-log readers). `Kube` and `Serialization`
    /// already only describe the operator's own managed objects, so their
    /// `Display` text is safe to surface as-is. `AppDefaults` messages can
    /// include internal `image-defaults.toml` config facts (#377), so they
    /// get a generic message instead -- the full detail is still captured by
    /// the `warn!(%error, ...)` structured log at the point of failure in
    /// `error_policy`.
    pub fn tenant_message(&self) -> String {
        match self {
            Error::Kube(_) | Error::Serialization(_) => self.to_string(),
            Error::AppDefaults(_) => {
                "internal error resolving app defaults; see operator logs for details"
                    .to_string()
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests to confirm they pass**

```bash
cargo test -p servarr-operator --lib tenant_message
cargo test --workspace
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat: add Error::tenant_message with a per-variant Event-visibility policy

refs #377
EOF
)"
```

---

### Task 7: `error_policy` publishes `tenant_message`, not raw `Display` text (#377)

**Files:**
- Modify: `crates/servarr-operator/src/controller.rs:1491-1515` (`error_policy`)

**Interfaces:**
- Consumes: `Error::tenant_message` from Task 6.
- Produces: no signature change — `error_policy`'s own signature and `Action::requeue(Duration::from_secs(60))` return value are unchanged, so `crates/servarr-operator/tests/reconcile_tests.rs::test_error_policy_returns_requeue_60s` continues to pass unmodified.

- [ ] **Step 1: Implement**

In `crates/servarr-operator/src/controller.rs`, change `error_policy`:

```rust
pub fn error_policy(app: Arc<ServarrApp>, error: &Error, ctx: Arc<Context>) -> Action {
    let app_type = app.spec.app.as_str();
    increment_reconcile_total(app_type, "error");
    warn!(%error, "reconciliation failed, requeuing");

    let recorder = Recorder::new(ctx.client.clone(), ctx.reporter.clone());
    let obj_ref = app.object_ref(&());
    let error_msg = error.tenant_message();
    tokio::spawn(async move {
        let _ = recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "ReconcileError".into(),
                    note: Some(error_msg),
                    action: "Reconcile".into(),
                    secondary: None,
                },
                &obj_ref,
            )
            .await;
    });

    Action::requeue(Duration::from_secs(60))
}
```

The only change is `let error_msg = error.to_string();` -> `let error_msg = error.tenant_message();`. The `warn!(%error, ...)` line above it is unchanged — `%error` still uses `Display` (the full, un-redacted text) for the operator's own structured log, which is exactly the "stays in operator logs" half of the policy.

- [ ] **Step 2: Run the existing test to confirm no regression**

```bash
cargo test -p servarr-operator --test reconcile_tests test_error_policy_returns_requeue_60s
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

Expected: all green. `test_error_policy_returns_requeue_60s` uses `Error::Serialization`, whose `tenant_message` equals `to_string()` (Task 6), so its behavior (and the `Action::requeue` assertion) is unaffected.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
fix: error_policy no longer publishes raw AppDefaults error text to tenant Events

Fixes #377
Fixes #386
EOF
)"
```

---

## Post-plan checklist (not a task — reference for the orchestrating skill)

- `cargo test --workspace`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check` all green on the final commit.
- `git log --oneline release/1.x..HEAD` shows `Fixes #378`, `Fixes #376`, `Fixes #377`, `Fixes #386` somewhere across the branch's commits.
- `docs/configuration.md` and the two regenerated CRD chart templates are part of the diff (Task 5).
