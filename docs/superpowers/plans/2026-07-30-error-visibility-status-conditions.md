# Error Visibility in Status Conditions — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop real error detail from being lost before it reaches a tenant-visible Kubernetes
Event or `.status.conditions[]` — fix three independent instances of the same failure class:
flattened SDK error status, raw unsanitized error text, and silently swallowed deserialization
errors.

**Architecture:** No new abstractions beyond what each site already has established:
`ApiError::log_summary()` (crates/servarr-api/src/client.rs) already exists and is the sanitization
pattern for HTTP response bodies that may echo credentials. This plan extends the same idea to
(a) SDK-generated error types losing their real HTTP status, and (b) `kube::Error`/`SecretError`
raw text reaching status Conditions — by adding a small, local trait and a `SecretError::log_summary()`
mirroring the existing `ApiError::log_summary()`. Task 3 is unrelated to sanitization — it's a
plain "stop swallowing a `Result::Err` as `None`" fix.

**Tech Stack:** Rust, `thiserror`, `kube-rs`, generated OpenAPI SDK crates (`sonarr`, `radarr`,
`lidarr`, `prowlarr`, `overseerr`).

## Global Constraints

- Workspace lints deny `unwrap_used`/`panic` — no `.unwrap()`/`.expect()` in non-test code.
- `cargo fmt` after every edit; `cargo clippy --all-targets --all-features -- -D warnings` must be
  clean (CI runs Rust 1.94.0 — stricter than local toolchain on some lints, e.g.
  `clippy::bool_comparison`; see project CLAUDE.md).
- Lib crates use `tracing` only, never `println!`/`eprintln!`.
- Every new/changed public error-producing function keeps its existing call-site signature where
  possible — these are called from many places; prefer additive fixes (new trait impl, new method)
  over call-site churn.
- Closes #421 (composite), #406, #407, #399 on merge — do not touch commit messages until Step 6b
  of the ship-issue skill (auto-close references go in the final commit body, not per-task commits).

---

### Task 1: Capture the real HTTP status in SDK error mapping (#406)

**Files:**
- Modify: `crates/servarr-api/src/servarr_v3.rs:120-125` (`map_sdk_err`) and its test at line 556
  (`map_sdk_err_formats_debug`)
- Modify: `crates/servarr-api/src/prowlarr.rs:5-9` (`map_sdk_err`) — same bug, separate function,
  not named in #406's issue text but is the identical pattern in the same crate; fix it too rather
  than leave a known duplicate.
- Modify: `crates/servarr-api/src/overseerr.rs:19-23` (`map_err`) and line 128 (inline
  `ApiError::ApiResponse { status: 0, .. }` in `setup_local_auth`) — same bug pattern, different
  app. Fix all three so the "status: 0 on every SDK-backed error" bug doesn't linger in the two
  files #406 didn't name.

**Interfaces:**
- Produces: `ApiError::ApiResponse { status, body }` now carries the real upstream HTTP status
  when the underlying SDK error is a response error, `0` only when there genuinely was no HTTP
  response (network/transport/serde error on the client side).
- No signature changes to any of the three `map_*` functions or `setup_local_auth` — callers are
  unaffected.

**Background — why a trait, not per-app functions:**

`crates/servarr-api/src/servarr_v3.rs::map_sdk_err` is `fn map_sdk_err<E: std::fmt::Debug>(e: E) -> ApiError`,
called via `.map_err(map_sdk_err)` at ~20 sites across `sonarr::apis::Error<T>`,
`radarr::apis::Error<T>`, `lidarr::apis::Error<T>`, `prowlarr::apis::Error<T>` — four distinct
generated types (different crates), each itself generic over a per-endpoint `T`. All four crates'
generated `Error<T>` enum has this identical shape (verified against the `sonarr` crate source,
`~/.cargo/registry/src/*/sonarr-0.1.1/src/apis/mod.rs`):

```rust
pub struct ResponseContent<T> {
    pub status: reqwest::StatusCode,
    pub content: String,
    pub entity: Option<T>,
}

pub enum Error<T> {
    Reqwest(reqwest::Error),
    Serde(serde_json::Error),
    Io(std::io::Error),
    ResponseError(ResponseContent<T>),
}
```

There's no shared trait across the four generated crates to match on, so define one locally in
`servarr_v3.rs` and implement it once per crate (4 impls, each still generic over the per-endpoint
`T`) — this keeps every existing `.map_err(map_sdk_err)` call site unchanged (type inference still
resolves `E` to the concrete SDK error type).

- [ ] **Step 1: Write the failing test for `servarr_v3.rs`**

Replace the existing test (it currently passes a bare `&str`, which will no longer compile once
`map_sdk_err` gains a trait bound — a bare `&str` can't implement `SdkResponseStatus`). Replace
`map_sdk_err_formats_debug` at line ~556 with two tests that exercise the real SDK error type:

```rust
#[test]
fn map_sdk_err_preserves_response_status() {
    let response_err = sonarr::apis::Error::ResponseError(sonarr::apis::ResponseContent {
        status: reqwest::StatusCode::UNAUTHORIZED,
        content: "invalid api key".to_string(),
        entity: None::<()>,
    });
    let err = map_sdk_err(response_err);
    match err {
        ApiError::ApiResponse { status, body } => {
            assert_eq!(status, 401);
            assert!(body.contains("invalid api key"));
        }
        other => panic!("expected ApiResponse, got {other:?}"),
    }
}

#[test]
fn map_sdk_err_falls_back_to_zero_for_non_response_errors() {
    // Serde/Io/Reqwest variants never carry a real HTTP status — 0 is correct here,
    // not a bug: it accurately means "no HTTP response was involved".
    let serde_err: sonarr::apis::Error<()> =
        sonarr::apis::Error::Serde(serde_json::from_str::<()>("not json").unwrap_err());
    let err = map_sdk_err(serde_err);
    match err {
        ApiError::ApiResponse { status, .. } => assert_eq!(status, 0),
        other => panic!("expected ApiResponse, got {other:?}"),
    }
}
```

Check the exact field names on `sonarr::apis::ResponseContent` and `sonarr::apis::Error` against
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sonarr-0.1.1/src/apis/mod.rs` before
writing — the generated crate version is pinned in `crates/servarr-api/Cargo.toml` (`sonarr = "0.1.1"`)
and should match, but confirm rather than assume.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p servarr-api map_sdk_err --lib`
Expected: compile error (trait bound not yet added) or, once it compiles, `map_sdk_err_preserves_response_status` FAILS with `status == 0` instead of `401`.

- [ ] **Step 3: Implement the trait and fix `map_sdk_err` in `servarr_v3.rs`**

Add near the top of `servarr_v3.rs`, above `map_sdk_err`:

```rust
/// Extracts the real HTTP status from a generated OpenAPI SDK error, when one exists.
///
/// The four generated `apis::Error<T>` types (sonarr/radarr/lidarr/prowlarr) are structurally
/// identical but not the same Rust type, so this trait unifies them for `map_sdk_err` without
/// forcing every `.map_err(map_sdk_err)` call site to name the concrete SDK crate.
trait SdkResponseStatus {
    fn response_status(&self) -> Option<u16>;
}

impl<T> SdkResponseStatus for sonarr::apis::Error<T> {
    fn response_status(&self) -> Option<u16> {
        match self {
            Self::ResponseError(rc) => Some(rc.status.as_u16()),
            _ => None,
        }
    }
}

impl<T> SdkResponseStatus for radarr::apis::Error<T> {
    fn response_status(&self) -> Option<u16> {
        match self {
            Self::ResponseError(rc) => Some(rc.status.as_u16()),
            _ => None,
        }
    }
}

impl<T> SdkResponseStatus for lidarr::apis::Error<T> {
    fn response_status(&self) -> Option<u16> {
        match self {
            Self::ResponseError(rc) => Some(rc.status.as_u16()),
            _ => None,
        }
    }
}

impl<T> SdkResponseStatus for prowlarr::apis::Error<T> {
    fn response_status(&self) -> Option<u16> {
        match self {
            Self::ResponseError(rc) => Some(rc.status.as_u16()),
            _ => None,
        }
    }
}
```

Then change `map_sdk_err`:

```rust
fn map_sdk_err<E: std::fmt::Debug + SdkResponseStatus>(e: E) -> ApiError {
    let status = e.response_status().unwrap_or(0);
    ApiError::ApiResponse {
        status,
        body: format!("{e:?}"),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p servarr-api map_sdk_err --lib`
Expected: both new tests PASS.

- [ ] **Step 5: Apply the identical fix to `prowlarr.rs`'s own `map_sdk_err`**

This one is already scoped to a single crate (`prowlarr::apis::Error<T>`), so no trait is needed —
match directly:

```rust
fn map_sdk_err<T: std::fmt::Debug>(e: prowlarr::apis::Error<T>) -> ApiError {
    let status = match &e {
        prowlarr::apis::Error::ResponseError(rc) => rc.status.as_u16(),
        _ => 0,
    };
    ApiError::ApiResponse {
        status,
        body: format!("{e:?}"),
    }
}
```

Add an analogous pair of tests in `prowlarr.rs`'s test module (response-status-preserved,
non-response-falls-back-to-zero), mirroring Step 1's tests but using `prowlarr::apis::Error`
directly (no trait needed since the function is already concretely typed).

- [ ] **Step 6: Apply the identical fix to `overseerr.rs`**

`map_err` is already typed to `overseerr::apis::Error<E>` — same direct-match fix as Step 5:

```rust
fn map_err<E: std::fmt::Debug>(e: overseerr::apis::Error<E>) -> ApiError {
    let status = match &e {
        overseerr::apis::Error::ResponseError(rc) => rc.status.as_u16(),
        _ => 0,
    };
    ApiError::ApiResponse {
        status,
        body: format!("{e:?}"),
    }
}
```

Also fix the inline `ApiError::ApiResponse { status: 0, body: e.to_string() }` in
`setup_local_auth` (line ~128) — this one wraps a `reqwest::Error` directly (not the generated
SDK's `Error<T>`), which never carries a meaningful HTTP status of its own (the actual non-2xx
response is handled separately, a few lines below, via `resp.status().as_u16()`). Leave `status: 0`
here but add a one-line comment explaining why it's correct as-is:

```rust
.map_err(|e| ApiError::ApiResponse {
    // reqwest::Error here means the request itself failed to complete (DNS, connect,
    // timeout) — there is no HTTP response to read a status from. 0 is accurate, not a bug.
    status: 0,
    body: e.to_string(),
})?;
```

Add tests for `map_err` mirroring Step 5's pattern, using `overseerr::apis::Error`.

- [ ] **Step 7: Run the full servarr-api test suite**

Run: `cargo test -p servarr-api`
Expected: all tests PASS, including the new ones and any pre-existing tests that referenced
`status: 0` behavior for genuinely non-response errors (those should still pass unchanged).

- [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt
cargo clippy -p servarr-api --all-targets --all-features -- -D warnings
git add crates/servarr-api/src/servarr_v3.rs crates/servarr-api/src/prowlarr.rs crates/servarr-api/src/overseerr.rs
git commit -m "$(cat <<'EOF'
fix: preserve real HTTP status in SDK error mapping

map_sdk_err (servarr_v3.rs, prowlarr.rs) and overseerr.rs's map_err hardcoded
status: 0 for every error from the generated OpenAPI SDK clients, so
ApiError::log_summary() could never report the real upstream status (401,
404, 500, ...) for Sonarr/Radarr/Lidarr/Prowlarr/Overseerr SDK-backed calls.
Extract the real status from the SDK's ResponseError variant when present;
0 remains correct for genuine non-HTTP errors (transport/serde/IO).

refs #406
EOF
)"
```

(`refs #406`, not `Fixes` — the auto-close reference goes in the final PR-closing commit per the
ship-issue skill's Step 6b; this task's commit just needs traceability.)

---

### Task 2: Sweep raw `kube::Error`/`SecretError` leaks into status Conditions (#407)

**Files:**
- Modify: `crates/servarr-api/src/k8s.rs` — add `SecretError::log_summary()`
- Modify: `crates/servarr-operator/src/controller.rs` — ~15 sites (see classification table below),
  plus a shared `kube_err_summary` helper
- Modify: `crates/servarr-crds/src/v1alpha1/defaults.rs` — one doc comment (no behavior change)

**Interfaces:**
- Consumes: `ApiError::log_summary()` (Task 1 unaffected; this task doesn't touch `ApiError`).
- Produces: `SecretError::log_summary() -> String`, mirroring `ApiError::log_summary()`'s shape —
  used at the two `SecretError`-producing sites in `controller.rs`.
- Produces: `fn kube_err_summary(e: &kube::Error) -> String` (private to `controller.rs`) — used at
  every raw `kube::Error` site below.

**Background:** `crates/servarr-api/src/client.rs:31-36` already has the established pattern:

```rust
pub fn log_summary(&self) -> String {
    match self {
        Self::ApiResponse { status, .. } => format!("HTTP API error (status: {status})"),
        other => other.to_string(),
    }
}
```

`kube::Error`'s `Api(Box<Status>)` variant carries a Kubernetes `Status` object with `code` (HTTP
status) and `message`/`reason` fields set by the API server — lower risk than an upstream *arr
app's response body (which can echo submitted credentials — that's what `ApiError::log_summary`
guards against), but still infra detail (namespace/resource names, RBAC denial text) that
shouldn't land verbatim in a tenant-visible Condition. Reduce it the same way: keep the status
code, drop the free-text message/reason.

- [ ] **Step 1: Write the failing test for `kube_err_summary`**

Add near the top of `controller.rs`'s `#[cfg(test)] mod tests` (search for `mod tests` — it starts
around line 3217):

```rust
#[test]
fn kube_err_summary_drops_status_message_keeps_status_code() {
    // kube-client is pinned to 3.1.0 (verify against Cargo.lock if this ever drifts):
    // kube::Error::Api(Box<kube::core::Status>), Status { code: u16, message: String, ... }
    // and Status derives Default, so only the fields under test need setting.
    let status = kube::core::Status {
        code: 403,
        message: "secrets \"super-secret-name\" is forbidden: User cannot get".to_string(),
        reason: "Forbidden".to_string(),
        ..Default::default()
    };
    let err = kube::Error::Api(Box::new(status));
    let summary = kube_err_summary(&err);
    assert!(summary.contains("403"), "summary should keep the status code: {summary}");
    assert!(
        !summary.contains("super-secret-name"),
        "summary must not leak the raw API server message: {summary}"
    );
}
```

`kube::core::Status` and `kube::Error::Api`'s field type are pinned facts for `kube-client = "3.1.0"`
(confirmed in `Cargo.lock`) — re-verify against `~/.cargo/registry/src/*/kube-core-3.1.0/src/response.rs`
and `kube-client-3.1.0/src/error.rs` if `Cargo.lock` shows a different pinned version by the time
this task runs.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p servarr-operator kube_err_summary --lib`
Expected: compile error (function doesn't exist yet).

- [ ] **Step 3: Implement `kube_err_summary`**

Add to `controller.rs` near `result_to_condition` (~line 1324):

```rust
/// Returns a log-safe summary of a `kube::Error` that excludes the API server's free-text
/// message/reason, keeping only the HTTP status code when available.
///
/// `kube::Error::Api`'s `Status` can carry arbitrary API-server detail in `message`/`reason`
/// (resource names, RBAC denial text) — lower sensitivity than an upstream *arr app's response
/// body, but still infra detail that shouldn't land verbatim in a tenant-visible Condition. Every
/// other variant (transport, serde, discovery, config, ...) is bucketed to a single generic
/// string rather than enumerated — `kube::Error` gains variants across minor versions, and the
/// only one worth extracting structured detail from is `Api`'s status code; the rest never
/// carried anything as sensitive as an API-server message to begin with, and matching a wildcard
/// keeps this function correct without needing to track kube's variant list release to release.
fn kube_err_summary(e: &kube::Error) -> String {
    match e {
        kube::Error::Api(status) => format!("Kubernetes API error (status: {})", status.code),
        _ => "Kubernetes API error".to_string(),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p servarr-operator kube_err_summary --lib`
Expected: PASS.

- [ ] **Step 5: Add `SecretError::log_summary()`**

In `crates/servarr-api/src/k8s.rs`, mirror `ApiError::log_summary()`:

```rust
impl SecretError {
    /// Returns a log-safe summary. The `Kube` variant applies the same status-only reduction as
    /// `kube_err_summary` in the operator crate (duplicated rather than shared — `k8s.rs` doesn't
    /// depend on `servarr-operator`, and this is a two-line match, not worth a new shared crate);
    /// the other variants already only carry curated secret/key names, never external response
    /// content, so their `Display` is safe as-is.
    pub fn log_summary(&self) -> String {
        match self {
            Self::Kube(kube::Error::Api(status)) => {
                format!("Kubernetes API error (status: {})", status.code)
            }
            Self::Kube(_) => "Kubernetes API error".to_string(),
            other => other.to_string(),
        }
    }
}
```

Add a unit test in `k8s.rs`'s test module analogous to Step 1's, confirming `NoData`/`KeyNotFound`/
`InvalidUtf8` pass through unchanged (they're already safe — curated names only) and `Kube` drops
the message.

- [ ] **Step 6: Run k8s.rs tests**

Run: `cargo test -p servarr-api k8s --lib`
Expected: PASS.

- [ ] **Step 7: Apply the fix at every raw `kube::Error` site in `controller.rs`**

Re-run this to get current line numbers before editing (they will have shifted from the Task 1
edits and any reformatting):

```bash
rg -n 'anyhow::anyhow!\("[^"]*\{e\}' crates/servarr-operator/src/controller.rs
```

Classification (verified against the code as of this plan's writing — re-verify each against
current line numbers, since the fix touches ~15 of the ~27 matches):

| Site (context) | `e`'s type | Action |
|---|---|---|
| `"image-defaults.toml validation failed: {e}"` (~line 60) | `String` from `AppDefaults::validate_all()` | **Safe as-is** — curated internal string, add doc comment (Step 9), no code change |
| `"failed to scale down for restore: {e}"` (~1812) | `kube::Error` | **Fix**: wrap with `kube_err_summary(&e)` |
| `"restore succeeded but failed to remove annotation..."` (~1874) | `kube::Error` | **Fix**: wrap with `kube_err_summary(&e)` |
| `"failed to read API key for restore: {e}"` (~1901) | `SecretError` | **Fix**: use `e.log_summary()` |
| `"failed to load app defaults: {e}"` (~1905 and 9 more sites: ~2026, 2065, 2220, 2247, 2310, 2542, 2647, 3030, 3063) | `String` from `AppDefaults::for_app`/`try_for_app` | **Safe as-is** — same curated string, doc comment covers all, no code change |
| `"failed to create API client for restore: {e}"` (~1918) | `ApiError` from `ServarrClient::new` (only ever `InvalidUrl`/`InvalidApiKey`, never `ApiResponse`) | **Safe as-is** — add a one-line comment noting `ServarrClient::new` never returns a response-body-derived error, no code change |
| `warn!(... error = %e, "restore API call failed")` + Event `note` + `"restore API call failed: {e}"` (~1938-1953, three interpolations of the same `e`) | `ApiError` from `restore_backup()` (can be `ApiResponse` — this is a genuine credential-echo risk, the same class #405 fixed elsewhere) | **Fix all three**: use `e.log_summary()` |
| `warn!` + `"configure_sonarr({}) failed: {e}"` (~2575-2583) | `ApiError` from `bazarr_client.configure_sonarr()` | **Fix both**: use `e.log_summary()` |
| `warn!` + `"configure_radarr({}) failed: {e}"` (~2588-2596) | `ApiError` from `bazarr_client.configure_radarr()` | **Fix both**: use `e.log_summary()` |
| `warn!` + `"disable_sonarr failed: {e}"` (~2599) | `ApiError` from `bazarr_client.disable_sonarr()` | **Fix both**: use `e.log_summary()` |
| `warn!` + `"disable_radarr failed: {e}"` (~2603) | `ApiError` from `bazarr_client.disable_radarr()` | **Fix both**: use `e.log_summary()` |
| `error!` + `"list_sonarr failed: {e}"` (~2745-2749) | `ApiError` from `maintainerr_client.list_sonarr()` | **Fix both**: use `e.log_summary()` |
| `error!` + `"list_radarr failed: {e}"` (~2756-2760) | `ApiError` from `maintainerr_client.list_radarr()` | **Fix both**: use `e.log_summary()` |
| `"failed to list ServarrApps: {e}"` (~1991 and ~2918, two sites) | `kube::Error` from `Api::list` | **Fix both**: wrap with `kube_err_summary(&e)` |
| `"Jellyfin API key secret {jf_secret_name} unreadable: {e}"` (~2949) | `SecretError` | **Fix**: use `e.log_summary()` (keep `{jf_secret_name}` — that's a curated resource name, not response content) |
| `"failed to patch Subgen Deployment: {e}"` (~2996) | `kube::Error` from `deploy_api.patch()` | **Fix**: wrap with `kube_err_summary(&e)` |

For every "Fix" row, the pattern is: replace `{e}` in the `anyhow::anyhow!`/format string with the
sanitized value, e.g.:

```rust
// before
.map_err(|e| anyhow::anyhow!("failed to patch Subgen Deployment: {e}"))?;

// after
.map_err(|e| anyhow::anyhow!("failed to patch Subgen Deployment: {}", kube_err_summary(&e)))?;
```

And where a preceding `warn!`/`error!` log also interpolates the same raw `e` (several sites
above), fix that call too — same sanitized value, same reasoning (tracing output can end up in
log aggregation systems with broader access than the operator's own process).

- [ ] **Step 8: Run the full sweep test**

```bash
cargo build -p servarr-operator 2>&1 | tail -50
cargo test -p servarr-operator --lib
```
Expected: builds clean, all existing tests still PASS (no test asserted the old raw-error text, but
re-run to confirm nothing relied on it).

- [ ] **Step 9: Add the one doc comment covering the "safe as-is" sites**

In `crates/servarr-crds/src/v1alpha1/defaults.rs`, above `try_for_app` (~line 33) and `for_app` (the
sibling that likely calls it — check both), add:

```rust
/// # Error safety
/// The returned `Err(String)` is always built from curated, internal-only data — the app name
/// (a `ServarrApp.spec.app` enum variant) and static strings from this module. It never contains
/// user-supplied secrets, upstream API response bodies, or raw `kube::Error`/`reqwest::Error`
/// text, so callers may interpolate it directly into logs, Events, or status Conditions without
/// going through a `log_summary()`-style reduction.
```

This single comment is the "document why the raw text is safe to keep" acceptance criterion for
all 10 `"failed to load app defaults: {e}"` sites plus the `validate_all()` site — no per-call-site
comments needed (they'd be repetitive noise across 11 sites in `controller.rs`).

- [ ] **Step 10: Format, lint, commit**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
git add crates/servarr-operator/src/controller.rs crates/servarr-api/src/k8s.rs crates/servarr-crds/src/v1alpha1/defaults.rs
git commit -m "$(cat <<'EOF'
fix: sanitize kube::Error/SecretError before they reach status conditions

~15 sites in controller.rs interpolated a raw kube::Error or SecretError
into an anyhow::Error that eventually reaches a tenant-visible status
Condition or Event, exposing Kubernetes API server detail (resource names,
RBAC denial text) that arguably shouldn't be there. Added kube_err_summary()
(controller.rs) and SecretError::log_summary() (k8s.rs), mirroring the
existing ApiError::log_summary() pattern from #405 — both keep the HTTP
status code, drop the free-text message. The ~10 "failed to load app
defaults" sites were confirmed already-safe (curated internal string, no
external input) and left as-is with a doc comment explaining why.

refs #407
EOF
)"
```

---

### Task 3: Propagate deserialization errors in `httproute`/`tcproute` builders (#399)

**Files:**
- Modify: `crates/servarr-resources/src/httproute.rs:71`
- Modify: `crates/servarr-resources/src/tcproute.rs:73`
- Test: `crates/servarr-resources/tests/builder_tests.rs`

**Interfaces:**
- No signature change: both `build()` functions already return `Result<Option<DynamicObject>, String>`.
  Callers in `controller.rs` (lines ~462 and ~480) already do
  `servarr_resources::tcproute::build(&app).map_err(Error::AppDefaults)?` — the `?` already
  propagates a `Result::Err`, so fixing the builders is sufficient; no caller changes needed.

**Background:** Both builders currently end with:

```rust
Ok(serde_json::from_value(route).ok())
```

This silently turns a genuine deserialization failure (a bug in the hand-built `serde_json::json!`
document, or future schema drift) into `Ok(None)` — indistinguishable from "no route configured".
The fix is a straightforward `.map_err`:

```rust
Ok(Some(
    serde_json::from_value(route).map_err(|e| format!("failed to build HTTPRoute: {e}"))?,
))
```

(and `"failed to build TCPRoute: {e}"` for `tcproute.rs`). `serde_json::Error`'s `Display` is a
parse-location message (e.g. `"missing field `apiVersion`"`) — it's internal schema-drift detail
about our own hand-built JSON, not user data, so no sanitization is needed here (unlike Task 2).

- [ ] **Step 1: Write the failing test for `httproute.rs`**

Add to `crates/servarr-resources/tests/builder_tests.rs` (near the other `httproute::build` tests,
~line 578):

```rust
#[test]
fn test_httproute_build_propagates_deserialization_error() {
    // Can't easily corrupt the internal json! macro output from an external test, so this
    // test instead locks the *contract*: build() must return Result<Option<DynamicObject>, String>
    // where a Some(_) came from a successful deserialization, not swallowed Ok(None) on error.
    // The regression this guards against is structural (Ok(x.ok()) -> Ok(Some(x?))), verified
    // by the code-level assertion below plus the existing enabled/disabled tests still passing.
    let app = make_app_with_gateway_enabled(); // reuse existing test helper from this file
    let result = servarr_resources::httproute::build(&app);
    assert!(result.is_ok(), "valid gateway config must still build successfully: {result:?}");
    assert!(
        result.unwrap().is_some(),
        "valid gateway config must produce Some(route), not None"
    );
}
```

Check `builder_tests.rs` for the actual existing helper name that builds a `ServarrApp` with
gateway enabled (used by `test_httproute_builder_enabled` at ~line 585) and reuse it — don't invent
a new one. If no reusable helper exists, copy the `ServarrApp` construction from
`test_httproute_builder_enabled` directly into this test.

This test won't fail before the fix (the current code already returns `Some` for valid input — the
bug only manifests on deserialization failure, which is hard to trigger from valid `ServarrApp`
input by design). The value of this task is defense against *future* schema drift, not fixing an
observable-today bug; the "failing test" formality is satisfied by Step 3's compile-time check
instead — see Step 2.

- [ ] **Step 2: Verify the current code compiles with the old pattern, confirming baseline**

Run: `cargo test -p servarr-resources test_httproute_build_propagates_deserialization_error`
Expected: PASS (this confirms the happy path isn't broken by the test addition — the real
regression-guard is the code change in Step 3, verified by Step 4's full existing-test run).

- [ ] **Step 3: Implement the fix in `httproute.rs`**

```rust
// crates/servarr-resources/src/httproute.rs:71 — replace
Ok(serde_json::from_value(route).ok())
// with
Ok(Some(
    serde_json::from_value(route).map_err(|e| format!("failed to build HTTPRoute: {e}"))?,
))
```

- [ ] **Step 4: Implement the identical fix in `tcproute.rs`**

```rust
// crates/servarr-resources/src/tcproute.rs:73 — replace
Ok(serde_json::from_value(route).ok())
// with
Ok(Some(
    serde_json::from_value(route).map_err(|e| format!("failed to build TCPRoute: {e}"))?,
))
```

- [ ] **Step 5: Run the full existing builder test suite to confirm no regression**

Run: `cargo test -p servarr-resources --test builder_tests`
Expected: ALL existing httproute/tcproute tests still PASS — `test_httproute_builder_disabled`,
`test_httproute_builder_enabled`, `test_httproute_backend_uses_service_name_override`,
`test_tcproute_no_gateway_returns_none`, `test_tcproute_gateway_disabled_returns_none`,
`test_tcproute_http_route_no_tls_returns_none`, `test_tcproute_tcp_route_type_returns_some`,
`test_tcproute_http_route_with_tls_enabled_returns_some`,
`test_tcproute_parent_refs_with_namespace_and_section_name`,
`test_dynamic_object_serialization_preserves_type_meta`, `test_httproute_ssa_body_has_type_meta`
— none of these should change behavior since valid input still deserializes successfully; only the
error path changed.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt
cargo clippy -p servarr-resources --all-targets --all-features -- -D warnings
git add crates/servarr-resources/src/httproute.rs crates/servarr-resources/src/tcproute.rs crates/servarr-resources/tests/builder_tests.rs
git commit -m "$(cat <<'EOF'
fix: propagate HTTPRoute/TCPRoute deserialization errors instead of Ok(None)

httproute::build and tcproute::build ended with Ok(serde_json::from_value(route).ok()),
collapsing a genuine deserialization bug (future schema drift in the hand-built
serde_json::json! document) into Ok(None) — indistinguishable from "no route
configured". The caller in controller.rs treats None as "not applicable" and
silently produces no HTTPRoute/TCPRoute, with no error, log, or Event. Propagate
the error instead.

refs #399
EOF
)"
```

---

## Final Notes for the Implementer

- Tasks are independent (no file overlap) — implement in any order, but Task 1 first is
  recommended since it's the smallest and validates the general "confirm SDK crate field names
  before writing code" approach the other tasks also need.
- Every `map_err`/`log_summary`/`kube_err_summary` change in Tasks 1-2 is additive at the call
  site (swap what's interpolated, not the surrounding control flow) — resist the urge to refactor
  anything else nearby.
- Line numbers throughout this plan were read from the codebase at the time of writing (before any
  task's edits) — re-verify with `rg`/`grep` before editing each site, since earlier tasks' edits
  shift line numbers in the same file (Tasks 1 and 2 both touch files independently, but Task 2's
  own 15 sites shift relative to each other as edits land — work top-to-bottom or re-grep after
  each edit).
