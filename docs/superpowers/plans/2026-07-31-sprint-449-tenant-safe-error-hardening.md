# Sprint 449 — Type-safe tenant-facing errors: sanitizer hardening (#443, #444, #445)

## Context

Follow-ups to the #437/#438/#440 sanitizer sweep (PR #442, merged to `release/1.x` and
forward-merged to `main`). Goal: make the tenant-visible-message boundary type-safe so the
#407 → #427 → #435 → #440 leak class becomes unrepresentable instead of a review finding.

Domain context doc: `docs/superpowers/plans/2026-07-30-error-visibility-status-conditions.md`
— covers the PRIOR #406/#407/#399 composite (shipped as #437/#438/#440). Read it for the
history; this plan extends the same sanitizer surface.

Base branch: `release/1.x` (milestone v1.3.0; the #440 sanitizer code is already merged there).
Working branch: `feat/449-tenant-safe-error-hardening`.

## Global Constraints

Bind every task in this plan:

- Workspace lints deny `unwrap_used`, `panic`, `panic_in_result_fn`, `dbg_macro`, `todo` —
  never introduce any of them. `?` and proper error types only.
- `cargo fmt` after every edit. `cargo clippy --all-targets --all-features -- -D warnings`
  must be clean locally before the task is done. CI runs Rust 1.94.0, which is stricter than
  the local toolchain (known stricter lint: `clippy::bool_comparison`) — run clippy with
  `-D warnings` locally to catch regressions early.
- `servarr-api` is a lib crate: `tracing` only, never `println!`/`eprintln!` (denied anyway).
- Prefer additive fixes (new trait impl / new method) over call-site churn.
- TDD mandatory: write the failing test first, watch it fail, then make it pass.
- Newtype pattern (project rules): private inner field, derive `Debug, Clone, PartialEq, Eq`,
  `new()` / `From` impls as the single construction path, `AsRef<str>` + `Display`.
- New public types must be re-exported from `crates/servarr-api/src/lib.rs`.
- Commit per task with a plain, imperative conventional-commit subject. Do NOT add closing
  refs ("Fixes #…"/"Closes #…") to commit bodies — the PR body carries the closing refs.
  Do NOT add attribution trailers (project commit-msg hook blocks them).
- Do not touch `Cargo.toml` version numbers (cargo-release owns them).
- The three tasks are tightly coupled and MUST land in one PR. Task 2 depends on Task 1's
  types; Task 3 reuses the sanitizers but is otherwise independent. Implement in order.

## Task 1 — Make `result_to_condition` type-safe via a `TenantSafeMessage` newtype (#443)

**Goal.** Today `result_to_condition<E: std::fmt::Display>` formats the error's `Display` into
a tenant-visible status `Condition` message verbatim. Any error type that happens to satisfy
`Display` — including `anyhow::Error` wrapping a `kube::Error::Api` whose message leaks the
API server's free-text message/reason — can reach the Condition. Make that class
unrepresentable: the only values that can be turned into a Condition message are those the
sanitizers produce.

### 1a. Add `TenantSafeMessage` to `servarr-api`

New type in `crates/servarr-api/src/` (add `mod` + re-export in `lib.rs`). Signature:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantSafeMessage(String);
```

Construction paths:

- `pub fn new(msg: impl Into<String>) -> Self` — the single construction path. **Contract:**
  pass only (a) static operator text, (b) values the tenant owns (resource/secret names,
  namespace names, typed enum variants), or (c) output already produced by a sanitizer
  (`public_summary()` / `log_summary()` / `kube_err_public_summary()`). Never raw `Display`
  of a `kube::Error` / `reqwest::Error` / `ApiError` or any error carrying external content,
  and never API response bodies. This constructor exists because Task 1c's functions contain
  operator-authored static messages ("Maintainerr service spec has no ports", "{N} app(s)
  failed to sync…") and prefix-wrapped sanitized summaries ("failed to read Prowlarr API key:
  {public_summary}") — with no constructor at all those sites would be unexpressible.
- `impl From<kube::Error> for TenantSafeMessage` → `Self::new(kube_err_public_summary(&e))`
- `impl From<SecretError> for TenantSafeMessage` → `Self::new(e.public_summary())`
- `impl From<ApiError> for TenantSafeMessage` → `Self::new(e.log_summary())` (ApiError::log_summary
  is already the tenant-safe summary for ApiError, per the #440 sweep)

**MUST NOT** implement `From<String>` or `From<&str>` or `From<anyhow::Error>`. The point is
that a raw string or an untyped error bundle cannot silently `.into()`. `new()` is an
*explicit, reviewable call* at each site, whereas `From` would convert silently anywhere —
including inside `result_to_condition`'s `E: Into<TenantSafeMessage>` bound, where `String`
would satisfy the bound and reopen the leak. `new()` does not:
`result_to_condition(Err(raw_string), …)` still fails to compile. (Resolution of an internal
plan conflict: 1a forbade any string constructor while 1c's functions contain messages that
cannot be built from a sanitizer alone — the named `new()` resolves it, preserving the
no-silent-conversion guarantee.)

Accessors:

- `impl AsRef<str> for TenantSafeMessage` (newtype convention; `AsRef` over `Deref`)
- `impl std::fmt::Display for TenantSafeMessage` → formats the inner string (needed so the
  Condition message / Event message can be produced with `%`)

Construction note: to avoid `String::from(...).into()` ambiguity, give the struct a private
`fn from_sanitized(s: String) -> Self` and have each `From` impl call it.

### 1b. Change `result_to_condition` to take `E: Into<TenantSafeMessage>`

In `crates/servarr-operator/src/controller.rs`, `result_to_condition` (line ~1351) changes
its generic bound from `E: std::fmt::Display` to `E: Into<TenantSafeMessage>`:

```rust
Err(e) => {
    let msg: TenantSafeMessage = e.into();
    warn!(%name, error = %msg, "{}", spec.fail_log);
    Condition::fail(spec.condition_type, spec.fail_reason, msg.as_ref(), now)
}
```

Note the log line cannot keep logging `%e` on the generic `E: Into<TenantSafeMessage>` (no
`Display` bound — `%e` would not compile), so it logs the sanitized `%msg` instead. The full
operator detail is already captured by the internal `warn!` calls inside the 6 functions at
the error origin (they log `error = %e.public_summary()` / `log_summary()` / `kube_err_summary()`
before returning `Err`). Convert first, then log; `e.into()` consumes `e`.

### 1c. Change the 6 sync/restore functions to return `Result<(), TenantSafeMessage>`

Currently all 6 return `Result<(), anyhow::Error>`:

- `maybe_restore_backup` (line 1796)
- `sync_prowlarr_apps` (line 2098)
- `sync_overseerr_servers` (line 2374)
- `sync_bazarr_apps` (line 2624)
- `sync_maintainerr_servers` (line 2734)
- `sync_subgen_jellyfin` (line 3015)

Change each return type to `Result<(), TenantSafeMessage>` and adapt the bodies so every
internal `?` on a `kube::Error`, `SecretError`, or `ApiError` auto-converts via the new
`From` impls, and any `anyhow::Error` or `String`-carrying error is explicitly mapped to a
sanitized `TenantSafeMessage` at the boundary (this is the forcing function that makes leaks
a compile error). Use `::map_err(...)` at the boundary points where an error type has no
`From` impl.

**This is the core of the task.** The type change is what makes the leak class
unrepresentable. Audit every error that currently flows through these functions and give each
a sanitized mapping. Any place that previously formatted an error into a message that reached
the Condition must now route through a sanitizer or be collapsed to a generic message.

Implementation directives:

- **Construction pattern.** Every `anyhow::anyhow!(...)` in these functions becomes
  `TenantSafeMessage::new(format!(...))` with the **exact same string bytes** — the golden
  test (1e) and the existing tenant-sanitized tests require byte-identity. The sanitizer calls
  already inside the format strings (`e.public_summary()`, `e.log_summary()`,
  `kube_err_public_summary(&e)`) stay exactly as they are. The curated `AppDefaults::for_app`
  error (`Result<_, String>`) is documented safe to interpolate (`defaults.rs` "Error safety"
  note) — keep `{e}` in the format string.
- **Helper return types.** `discover_namespace_apps` (line 2032, `Result<_, anyhow::Error>`)
  is called by 4 of the 6 functions via bare `?`; change it to
  `Result<Vec<DiscoveredApp>, TenantSafeMessage>` (its 2 internal `map_err` closures already
  produce sanitized text — swap `anyhow::anyhow!(...)` for `TenantSafeMessage::new(format!(...))`)
  so the call sites keep bare `?`. Same for `try_restore` (line 1926) if `maybe_restore_backup`
  propagates it via `?` (it is only called from there + one test). Update the affected unit
  tests — the error-message strings must stay byte-identical.
- **Accumulators.** `sync_bazarr_apps`'s `first_error: Option<anyhow::Error>` (line 2661)
  becomes `Option<TenantSafeMessage>`; the `get_or_insert_with(|| anyhow::anyhow!(...))`
  closures become `TenantSafeMessage::new(format!(...))`.
- **Do NOT change the cleanup functions** (`cleanup_prowlarr_registration` line 2277,
  `cleanup_overseerr_registration` line 3148). They stay `Result<(), anyhow::Error>` with
  their log-only sanitizers — Task 3 (#444) owns their Event emission.
- **Static messages are fine under `new()`** when they interpolate only tenant-owned values:
  "no Jellyfin CR found in namespace {target_ns}", "{failures} app(s) failed to sync into
  Maintainerr (see warnings above)", "Maintainerr service spec has no ports; check
  spec.service or app defaults", "Prowlarr sync requires api_key_secret", etc.

### 1d. Compile-fail regression test

Because the goal is that a raw error CANNOT be converted, add a rustdoc `compile_fail` doctest
in `servarr-api` (no trybuild in the workspace — rustdoc `compile_fail` is the established
mechanism). It must demonstrate that a raw `String` / `&str` cannot silently turn into a
`TenantSafeMessage`:

````rust
/// ```compile_fail
/// use servarr_api::TenantSafeMessage;
/// // A raw String must not silently convert into a TenantSafeMessage:
/// let _ = TenantSafeMessage::from("raw untrusted string".to_string());
/// // Same for &str:
/// let _ = TenantSafeMessage::from("raw untrusted string");
/// ```
````

Verify the doctest FAILS to compile (that's the pass condition) when the API has no
`From<String>` / `From<&str>` — i.e. it must currently fail. If it currently compiles, that's
a bug in your type design.

Note: a raw `kube::Error` is deliberately NOT the compile_fail target — `From<kube::Error>`
exists by design, routing through `kube_err_public_summary`. The leak class is a raw
`String`/`&str`/`anyhow::Error` bypassing the sanitizers entirely. (`anyhow` is not a dep of
`servarr-api`, so it cannot appear in a servarr-api doctest; the operator crate's `?`-and-
`new()` discipline covers the anyhow case, and `From<anyhow::Error>` is simply never
implemented.)

### 1e. Golden test: today's Condition messages are byte-identical

Add a test in `controller.rs` that asserts the sanitized message for a representative error
equals today's expected Condition message. The point: `result_to_condition` behavior for the
*sanitized* path is unchanged — an `ApiResponse { status: 401, body }` produces
`HTTP API error (status: 401)` as the Condition message, exactly as it does today. Capture the
existing expected strings (look at the current tests around line 4580,
`sync_prowlarr_apps_api_key_read_error_is_tenant_sanitized` etc. for the established
expectations).

The existing tenant-sanitized tests around line 4580 (and the `try_restore`/`discover_namespace_apps`
error tests) ARE the golden test — they encode today's messages and must pass **unchanged**
after the refactor. If any of them must be edited to match new output, that is a
behavior-change signal: stop and flag it before proceeding.

### 1f. All call sites still compile

`result_to_condition` is called at lines 267, 562, 586, 610, 634, 658 (the sync/restore
conditions). With the 6 functions returning `Result<(), TenantSafeMessage>` those call sites
resolve automatically. If any call site passes a different error type, adapt it with an
explicit `map_err` to `TenantSafeMessage`.

### 1g. Test list

- Unit tests for `TenantSafeMessage` `From` impls: `From<kube::Error::Api(403)>` →
  contains "403", NOT the API message; `From<SecretError>` → `public_summary` value;
  `From<ApiError>` → `log_summary` value.
- `compile_fail` doctest as above.
- Golden Condition-message test as above.
- Full `cargo test` green; `cargo clippy --all-targets --all-features -- -D warnings` clean.

## Task 2 — Property-based tests for the error sanitizers (#445)

**Goal.** Proptest strategies construct every `ApiError` variant and the detail-carrying
`kube::Error` / `SecretError` variants, and assert the "no seed substring" property holds for
the tenant-safe paths — the sanitizers never leak the sensitive input (API key, secret, secret
name, response body, URL) into their output.

Location: `crates/servarr-api/src/k8s.rs` and `crates/servarr-api/src/client.rs` test modules,
next to the sanitizer definitions. `proptest = { workspace = true }` is already a dev-dep of
`servarr-api`.

### 2a. Strategies

Construct every variant:

- `ApiError` (client.rs): `Request(reqwest::Error)`, `InvalidUrl(url::ParseError)`,
  `ApiResponse { status, body }`, `InvalidApiKey`, `OperationFailed { message }`.
  - `Request` needs a real `reqwest::Error` (no public constructor). Use the same technique as
    the existing test `log_summary_hides_url_for_request_error`: a `HttpClient` pointed at
    `http://127.0.0.1:1` and a failing GET to produce a `Request` error at runtime, then feed
    the *seed* (a credential-bearing query string) into that construction. This makes the
    `Request` strategy a runtime-produced value rather than a pure proptest strategy — handle
    it as a separate test, not a pure strategy, OR document why it can't be a pure strategy.
  - The other variants are pure: arbitrary `status: u16`, arbitrary body string seeded with a
    known token, arbitrary `OperationFailed` message seeded with a known token.
- `kube::Error` detail-carrying variants (k8s.rs): `Api(Box<Status>)` with a seeded message;
  plus the non-`Api` detail carriers that `kube_err_public_summary` must collapse:
  `LinesCodecMaxLineLengthExceeded` (a unit variant, already used in existing tests) and
  `SerdeError`/`Auth` if constructible. For variants that are unit/plain, assert the public
  summary equals the generic string.
- `SecretError`: `Kube(kube::Error)` (delegate to the kube strategy), `NoData { name }`,
  `KeyNotFound { name, key }`, `InvalidUtf8 { name, key }` — names/keys seeded with a known
  token for the no-leak property.

### 2b. Properties

- **No-seed-substring**: for each tenant-safe path (`ApiError::log_summary`,
  `SecretError::public_summary`, `kube_err_public_summary`), a sensitive token inserted into
  the underlying error (response body, message, secret name/key, URL) NEVER appears as a
  substring of the summary. Seeds: a fixed recognizable token like `"SEED-SECRET-TOKEN"` plus
  a random string per case.
- **Charset allowlist**: every sanitizer output matches `^[A-Za-z0-9 ._()-]*$`. This is the
  invariant that the tenant-visible message can't smuggle arbitrary content through.
- **Status-code preservation** (kube `Api` and `ApiResponse`): the summary contains the
  numeric status code.
- **Non-`Api` kube variants collapse**: `kube_err_public_summary` of a non-`Api` variant is
  exactly `"Kubernetes client error"`.

### 2c. Test list

- proptest tests in `k8s.rs`: kube `Api` seeded-message no-leak + status-code property;
  non-`Api` collapse property; `SecretError::public_summary` no-leak for all four variants.
- proptest tests in `client.rs`: `ApiError::log_summary` no-leak for `ApiResponse` (body seed),
  `OperationFailed` (message seed), `InvalidUrl`; charset allowlist for all.
- Runtime `Request`-error no-leak test (existing technique, extended if needed).
- `cargo test` green; clippy clean.

## Task 3 — Emit a Kubernetes Event when finalizer cleanup fails (#444)

**Goal.** When finalizer cleanup fails in `cleanup_prowlarr_registration` /
`cleanup_overseerr_registration`, emit a `Warning` Event (reason `CleanupFailed`) with a
tenant-safe message. Successful cleanup and skip paths produce NO events.

### 3a. Emit the Event on failure

Both cleanup functions already take `recorder: &Recorder` and `obj_ref: &ObjectReference`
(used for the success-path events if any). On the failure path, publish:

```rust
let _ = recorder.publish(Event::new(
    obj_ref.clone(),
    EventType::Warning,
    "CleanupFailed",
    &tenant_safe_message,
));
```

Details:
- Reason string is exactly `CleanupFailed` (stable UpperCamelCase, reusable so kube can
  aggregate repeated events).
- Message is tenant-safe: `TenantSafeMessage::from(kube_error).as_ref()` — i.e. reuse
  `kube_err_public_summary` via the Task 1 type. NOT the log-only `kube_err_summary`.
- Keep the existing `warn!` log line (full detail for operators) AND the `Err` propagation —
  the Event is an addition, not a replacement. The reconcile still returns the error.
- `publish` returns a `Result`; do NOT `?` on it (an Event-publish failure must not change
  reconcile behavior). Log at `warn!` on publish failure if desired — but do not let it
  change the returned error. The `let _ =` / `if let Err(pub_err) = ... { warn! }` pattern is
  fine.
- Only emit on ACTUAL failure. Check the current code: the cleanup functions may have paths
  that early-return Ok (nothing registered, already removed, skip). Those must NOT emit.

### 3b. Tests

- Failing-kube-error path: make the API call fail (mock via the existing recorder/test
  harness — check how the controller tests build a `Recorder` and a failing `Client`), assert
  an Event with type `Warning` and reason `CleanupFailed` was published, and assert the
  message is tenant-safe (does not contain the seeded raw message).
- Success path: assert NO event is published.
- If the cleanup function has a skip/early-return path: assert NO event there too.
- Check how existing controller tests capture published Events (there may be a
  `RecordingRecorder` or similar in the test harness — reuse it). Search the test module for
  `recorder` / `Event::` / `publish`.
- `cargo test` green; clippy clean.

## Acceptance Criteria (composite #449)

- **#443**: `result_to_condition` no longer accepts a bare `E: Display` that can carry
  untrusted detail; `TenantSafeMessage` newtype (private field) constructed only by the
  tenant-safe sanitizers; all existing call sites compile via explicit conversion;
  compile-fail regression test for a raw `kube::Error`; no behavior change to today's
  Condition messages (golden test).
- **#444**: Failed finalizer cleanup emits a `Warning` Event (reason `CleanupFailed`) with a
  tenant-safe message (not the log-only `kube_err_summary`); successful cleanup/skips produce
  no spurious Events; tests cover failing-kube-error and success paths.
- **#445**: Proptest strategies construct every `ApiError` variant and the detail-carrying
  `kube::Error` / `SecretError` variants; "no seed substring" property holds for the
  tenant-safe paths; tests live next to the sanitizer definitions.
