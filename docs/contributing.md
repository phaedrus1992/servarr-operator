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

The hooks run `cargo fmt`, `actionlint`, `zizmor`, `helm lint`, `cargo clippy`, and `cargo test`
on every commit.

## CI Commit Message Flags

The CI pipeline checks the commit message (on a push) or PR title (on a pull request) for
bracket-delimited flags that opt into expensive jobs. This keeps branch CI fast by default while
giving you an escape hatch when you need it.

| Flag | Effect |
|------|--------|
| `[full-build]` | Build the arm64 Linux binary in addition to the default amd64 build |
| `[smoke]` | Run the full integration smoke test against a live kind cluster |
| `[snapshot]` | Publish a snapshot container image and Helm chart from the branch |

**Examples:**

```
fix: correct Sonarr auth env var name [smoke]
```

```
feat: add arm64 support [full-build][smoke]
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
| smoke test | always | `[smoke]` only |
| snapshot publish | always | `[snapshot]` only |

Flags can be combined freely. `workflow_dispatch` runs treat all flags as enabled.

## Running the Smoke Test Locally

The smoke test requires Docker and `kubectl`:

```bash
# Start a kind cluster
kind create cluster

# Build a local image
cargo build --release --target x86_64-unknown-linux-musl --bin servarr-operator
docker build -t servarr-operator:dev -f- target/x86_64-unknown-linux-musl/release/ <<'EOF'
FROM gcr.io/distroless/static-debian12:nonroot
COPY servarr-operator /servarr-operator
USER nonroot:nonroot
ENTRYPOINT ["/servarr-operator"]
EOF

kind load docker-image servarr-operator:dev

# Install CRDs and operator
helm template smoke-crds charts/servarr-crds/ --set webhook.enabled=false | kubectl apply -f -
helm dependency build charts/servarr-operator/
helm template smoke charts/servarr-operator/ \
  --set image.repository=servarr-operator \
  --set image.tag=dev \
  --set image.pullPolicy=Never \
  --set webhook.enabled=false \
  --set watchAllNamespaces=true \
  | kubectl apply -f -

kubectl rollout status deployment/servarr-operator --timeout=120s
kubectl apply -f .github/smoke-test/manifests/
bash .github/smoke-test/smoke-test.sh
```
