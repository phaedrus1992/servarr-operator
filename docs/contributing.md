# Contributing

## Development Setup

```bash
git clone https://github.com/phaedrus1992/servarr-operator
cd servarr-operator
cargo build
cargo test
```

Pre-commit hooks are managed with [prek](https://github.com/nickel-lang/prek). Install once:

```bash
prek install
```

The hooks run `cargo fmt`, `actionlint`, `zizmor`, `helm lint`, `cargo clippy`, `cargo test`, and
`cargo hawk` (dead/unnecessary-pub lint) on every commit.

`cargo hawk` needs [cargo-hawk](https://github.com/astral-sh/hawk), which hooks compiler
internals and pins itself to whatever toolchain is active at install time (see
`rust-toolchain.toml`), so it also needs the `rustc-dev` and `llvm-tools-preview` components.
Install it once:

```bash
rustup component add rustc-dev llvm-tools-preview
RUSTC_BOOTSTRAP=1 cargo install --locked cargo-hawk
```

Deliberately unpinned — cargo-hawk hooks unstable rustc-internal APIs, so a binary built against
one toolchain often won't even compile against the next. Reinstall it after a `rust-toolchain.toml`
bump if `cargo hawk` starts failing to build.

`RUSTC_BOOTSTRAP=1` only scopes to that one install command — it does not affect `cargo build`,
`cargo test`, or any other command in your shell. The hook currently runs non-blocking (`-W`, not
`-D`); see #568/#569 for the tracked follow-up on tightening the findings it reports today.

## CI Commit Message Flags

The CI pipeline checks the commit message (on a push) or PR title (on a pull request) for
bracket-delimited flags that opt into expensive jobs. This keeps branch CI fast by default while
giving you an escape hatch when you need it.

| Flag | Effect |
|------|--------|
| `[full-build]` | Build the arm64 Linux binary in addition to the default amd64 build |
| `[snapshot]` | Publish a snapshot container image and Helm chart from the branch |

**Examples:**

```
feat: add arm64 support [full-build]
```

```
chore: update dependencies [snapshot]
```

### Default behaviour by branch

| Job | `main` | feature branch |
|-----|--------|---------------|
| lint (fmt, clippy, actionlint, zizmor, helm) | always | always |
| unit tests + coverage | always | always |
| amd64 Linux build | always | always |
| arm64 Linux build | always | `[full-build]` only |
| CRD drift check | always | always |
| smoke test | always | always |
| snapshot publish | always | `[snapshot]` only |

Flags can be combined freely. `workflow_dispatch` runs treat all flags as enabled.

## Coverage Gates

CI enforces two coverage gates. Both read one `cargo llvm-cov` JSON report, so coverage is
measured once per run.

| Gate | File | What it does |
|------|------|--------------|
| Aggregate | `.coverage-threshold` | Workspace line coverage must reach this percent. |
| Per file | `.coverage-floors` | Each file's line coverage must reach its own floor. |

The aggregate gate on its own can pass while one module sits far below the line and another
sits near 100%. The per-file floors catch that.

Run both gates locally:

```bash
mkdir -p .tmp
cargo llvm-cov --workspace --json --output-path .tmp/coverage.json
scripts/check-coverage.sh .tmp/coverage.json
```

On macOS, `cargo llvm-cov` finds Homebrew's `llvm-profdata`, which is newer than the Rust
profiler. It then fails with `raw profile version mismatch`. Point it at the toolchain's own
copy instead:

```bash
B="$HOME/.rustup/toolchains/$(rustc -vV | awk '/host/{print $2}')/lib/rustlib/$(rustc -vV | awk '/host/{print $2}')/bin"
LLVM_COV="$B/llvm-cov" LLVM_PROFDATA="$B/llvm-profdata" cargo llvm-cov --workspace --json --output-path .tmp/coverage.json
```

The variable names are `LLVM_COV` and `LLVM_PROFDATA`. Setting `PATH` does not work.

### Working with the floors

- A file with no entry of its own uses the `*` floor. A new module added without tests fails
  the gate rather than passing unnoticed.
- Each floor sits a couple of points below where the file measured when it was written, so a
  refactor that moves a line or two does not fail the build.
- The script reports files that sit well above their floor. Raise those floors deliberately.
- **Never lower a floor to make CI pass.** Add the missing tests instead.
- `main.rs` and `telemetry.rs` carry honest low floors. They are process wiring and are hard
  to unit-test. They stay in the report rather than being excluded, because dropping a file
  from the denominator raises the number by measuring less code.

## Running the Smoke Test Locally

The smoke test requires Docker, `kubectl`, and `helm`. It runs against any reachable cluster
(Docker Desktop, `kind`, `k3d`, Rancher Desktop) and builds the operator image via the repo's own
`Dockerfile`, so it always produces a binary matching the container's OS/arch regardless of your
host machine:

```bash
kind create cluster
scripts/smoke-test-local.sh
```

This is the same script CI runs (`smoke-test` job in `.github/workflows/ci.yaml`) and the local
pre-push hook invokes. Pass `--namespace NAME` for a fixed namespace, or `--keep` to leave the
namespace up for debugging instead of deleting it on exit.
