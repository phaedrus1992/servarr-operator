# Release Process Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `cargo-release` + Keep a Changelog release front-end to servarr-operator, and enrich the existing GitHub Release with a curated changelog, Helm install/upgrade instructions, and the auto-generated commit list.

**Architecture:** Releases stay tag-triggered (`release.yaml` on `v*`). `cargo-release` owns only local prep (bump one shared workspace version, roll `CHANGELOG.md`, bump both `Chart.yaml` files) in a single commit that lands via PR; the `v*` tag is cut on the merged commit. The existing `_publish.yaml` already builds the multi-arch image + Helm charts to GHCR; only its release-notes step changes.

**Tech Stack:** Rust workspace (Cargo, `cargo-release`), Bash (`shellcheck`/`shfmt`), GitHub Actions (`actionlint`/`zizmor`), Helm, `gh` CLI.

**Spec:** `docs/superpowers/specs/2026-06-17-release-process-design.md`

**Branch:** `feat/release-process` (already checked out). No prior `v*` tags exist; the first release will be `v0.1.0`.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `Cargo.toml` | Owns the single shared version (`[workspace.package] version`); correct `repository`. |
| `crates/*/Cargo.toml` (×4) | Inherit version via `version.workspace = true`. |
| `CHANGELOG.md` | Hand-curated, user-facing change log (Keep a Changelog). |
| `release.toml` | `cargo-release` config: prep-only, changelog rollover, chart bumps. |
| `scripts/changelog-extract.sh` | Print one version's changelog section; used by CI release notes + safety rail. |
| `.github/workflows/_publish.yaml` | Release-notes step builds the rich body. |
| `.github/workflows/release.yaml` | Safety-rail step (tag ⇔ version ⇔ changelog). |
| `charts/*/Chart.yaml` (×2) | `version`/`appVersion` become release-managed. |
| `RELEASING.md` | Human runbook for cutting a release. |

Task order is dependency-driven: version model → changelog → cargo-release config → extraction script → workflow notes → safety rail → runbook → end-to-end dry run.

---

## Task 1: Single shared workspace version

**Files:**
- Modify: `Cargo.toml` (`[workspace.package]`, lines 11-13)
- Modify: `crates/servarr-api/Cargo.toml:3`
- Modify: `crates/servarr-crds/Cargo.toml:3`
- Modify: `crates/servarr-resources/Cargo.toml:3`
- Modify: `crates/servarr-operator/Cargo.toml:3`

- [ ] **Step 1: Add shared version and fix repository in root `Cargo.toml`**

Replace the `[workspace.package]` block:

```toml
[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT"
repository = "https://github.com/phaedrus1992/servarr-operator"
```

- [ ] **Step 2: Switch each crate to inherit the version**

In each of the four `crates/*/Cargo.toml`, change line 3 from:

```toml
version = "0.1.0"
```

to:

```toml
version.workspace = true
```

- [ ] **Step 3: Verify the workspace still resolves**

Run: `cargo metadata --format-version 1 --no-deps >/dev/null && echo OK`
Expected: `OK` (no "failed to inherit" errors).

- [ ] **Step 4: Verify all crates report 0.1.0**

Run: `cargo metadata --format-version 1 --no-deps | grep -o '"name":"servarr-[a-z]*","version":"[^"]*"'`
Expected: every line shows `"version":"0.1.0"`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/*/Cargo.toml
git commit -m "build: use shared workspace version, fix repository url"
```

---

## Task 2: Add `CHANGELOG.md`

**Files:**
- Create: `CHANGELOG.md`

- [ ] **Step 1: Create the changelog skeleton**

Create `CHANGELOG.md` with exactly:

```markdown
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

<!-- next-header -->

## [Unreleased] - ReleaseDate

### Added

- Release automation: `cargo-release` + Keep a Changelog, with the multi-arch
  container image and Helm charts published to GHCR on each `v*` tag.

<!-- next-url -->
[Unreleased]: https://github.com/phaedrus1992/servarr-operator/compare/v0.1.0...HEAD
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: add Keep a Changelog CHANGELOG.md"
```

---

## Task 3: Add `release.toml`

> **Implementation note (actual):** This is a *virtual* workspace, so
> cargo-release runs `pre-release-replacements` once per crate with paths
> relative to each crate's manifest (it looked for `crates/servarr-api/CHANGELOG.md`
> and errored). The shared policy (`publish/tag/push=false`, `shared-version`,
> `allow-branch`, `tag-name`, commit message) stays in root `release.toml`, but
> the `pre-release-replacements` were moved to
> `crates/servarr-operator/Cargo.toml` `[package.metadata.release]` with `../../`
> paths so they run exactly once against the workspace-root files. Validated with
> `cargo release config` (confirms `publish=false`) and a dry run on a
> `release/*` branch.

**Files:**
- Create: `release.toml`
- Modify: `crates/servarr-operator/Cargo.toml` (add `[package.metadata.release]`)

- [ ] **Step 1: Create `release.toml`**

Create `release.toml` with exactly:

```toml
# cargo-release configuration.
# Releases are TAG-TRIGGERED: pushing a `v*` tag fires
# .github/workflows/release.yaml, which builds binaries and publishes the
# multi-arch image + Helm charts + GitHub Release.
#
# cargo-release owns only the local prep: bump the shared version, roll the
# CHANGELOG, and bump both Chart.yaml versions, in one commit. It does NOT
# publish, tag, or push:
#   - publish: there is no crates.io publish for this repo.
#   - tag/push: `main` is protected; the prep commit lands via PR and the `v*`
#     tag is cut on the merged commit. See RELEASING.md.

allow-branch = ["main", "release/*"]
publish = false
tag = false
push = false
shared-version = true
tag-name = "v{{version}}"
pre-release-commit-message = "chore(release): {{version}}"

pre-release-replacements = [
  # Keep a Changelog rollover (cargo-release FAQ pattern).
  { file = "CHANGELOG.md", search = "Unreleased", replace = "{{version}}", min = 1 },
  { file = "CHANGELOG.md", search = "\\.\\.\\.HEAD", replace = "...v{{version}}", exactly = 1 },
  { file = "CHANGELOG.md", search = "ReleaseDate", replace = "{{date}}", exactly = 1 },
  { file = "CHANGELOG.md", search = "<!-- next-header -->", replace = "<!-- next-header -->\n\n## [Unreleased] - ReleaseDate", exactly = 1 },
  { file = "CHANGELOG.md", search = "<!-- next-url -->", replace = "<!-- next-url -->\n[Unreleased]: https://github.com/phaedrus1992/servarr-operator/compare/v{{version}}...HEAD", exactly = 1 },

  # Keep both Helm charts honest in-repo (CI still passes versions at package time).
  { file = "charts/servarr-crds/Chart.yaml", search = "^version: .*$", replace = "version: {{version}}", exactly = 1 },
  { file = "charts/servarr-crds/Chart.yaml", search = "^appVersion: .*$", replace = "appVersion: \"{{version}}\"", exactly = 1 },
  { file = "charts/servarr-operator/Chart.yaml", search = "^version: .*$", replace = "version: {{version}}", exactly = 1 },
  { file = "charts/servarr-operator/Chart.yaml", search = "^appVersion: .*$", replace = "appVersion: \"{{version}}\"", exactly = 1 },
]
```

- [ ] **Step 2: Validate the config parses (dry run, no mutations)**

Run: `cargo release patch --dry-run 2>&1 | tail -30`
Expected: completes without error; output mentions updating `CHANGELOG.md` and both `Chart.yaml` files; no "Unrendered template" or "expected exactly 1" replacement errors. (Dry run makes no file changes — confirm with `git status --short` showing nothing.)

- [ ] **Step 3: If a Chart.yaml replacement reports a match-count error**

Only if Step 2 errors on a Chart.yaml replacement: cargo-release applies each `search` regex across the whole file. The anchored `^version: .*$` / `^appVersion: .*$` each match once per Chart.yaml (verified: each file has one of each). If the count is wrong, adjust the offending entry's `min`/`max`/`exactly` to match reality, then re-run Step 2. Do not loosen the anchors.

- [ ] **Step 4: Commit**

```bash
git add release.toml
git commit -m "build: add cargo-release configuration"
```

---

## Task 4: `scripts/changelog-extract.sh`

**Files:**
- Create: `scripts/changelog-extract.sh`

This script prints the body of one version's section from `CHANGELOG.md`
(everything after the `## [<version>]` header line up to, but excluding, the
next `## [` header or the `<!-- next-url -->` marker). Exit non-zero if the
section is absent. Used by both the release-notes step and the safety rail.

- [ ] **Step 1: Write the script**

Create `scripts/changelog-extract.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Print the changelog body for a given version.
# Usage: changelog-extract.sh <version> [changelog-path]
#   <version>        e.g. 1.2.3 (no leading v)
# Exits 1 if no matching "## [<version>]" section exists.
#
# --self-check runs an inline assertion on a fixture and exits.

extract() {
  local version="$1" file="$2"
  awk -v ver="$version" '
    BEGIN { found = 0 }
    # Header for the requested version: "## [1.2.3]" possibly followed by " - date"
    $0 ~ "^## \\[" ver "\\]" { found = 1; next }
    # Any later top-level section header, or the url-marker, ends the section.
    found && (/^## \[/ || /^<!-- next-url -->/) { exit }
    found { print }
  ' "$file"
}

self_check() {
  local tmp out
  tmp="$(mktemp)"
  trap 'rm -f "$tmp"' RETURN
  cat > "$tmp" <<'EOF'
# Changelog
<!-- next-header -->

## [1.2.0] - 2026-01-02

### Added

- Second thing

## [1.1.0] - 2026-01-01

### Added

- First thing

<!-- next-url -->
[Unreleased]: https://example/compare/v1.2.0...HEAD
EOF
  out="$(extract "1.2.0" "$tmp")"
  case "$out" in
    *"Second thing"*) ;;
    *) echo "self-check FAILED: missing current section body" >&2; exit 1 ;;
  esac
  case "$out" in
    *"First thing"*) echo "self-check FAILED: leaked into older section" >&2; exit 1 ;;
    *) ;;
  esac
  case "$out" in
    *"next-url"*|*"compare"*) echo "self-check FAILED: leaked url marker" >&2; exit 1 ;;
    *) ;;
  esac
  echo "self-check OK"
}

main() {
  if [ "${1:-}" = "--self-check" ]; then
    self_check
    exit 0
  fi
  if [ $# -lt 1 ]; then
    echo "usage: changelog-extract.sh <version> [changelog-path]" >&2
    exit 2
  fi
  local version="$1"
  local file="${2:-CHANGELOG.md}"
  local body
  body="$(extract "$version" "$file")"
  # Strip leading/trailing blank lines.
  body="$(printf '%s\n' "$body" | sed -e '/./,$!d' | tac | sed -e '/./,$!d' | tac)"
  if [ -z "$body" ]; then
    echo "no changelog section found for version $version in $file" >&2
    exit 1
  fi
  printf '%s\n' "$body"
}

main "$@"
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x scripts/changelog-extract.sh`

- [ ] **Step 3: Run the self-check (verify behavior)**

Run: `scripts/changelog-extract.sh --self-check`
Expected: `self-check OK`

- [ ] **Step 4: Run against the real changelog's Unreleased section is absent (negative case)**

Run: `scripts/changelog-extract.sh 9.9.9 CHANGELOG.md; echo "exit=$?"`
Expected: prints `no changelog section found for version 9.9.9 ...` to stderr and `exit=1`.

- [ ] **Step 5: Lint**

Run: `shellcheck scripts/changelog-extract.sh && shfmt -d scripts/changelog-extract.sh && echo LINT_OK`
Expected: `LINT_OK` (no diff from `shfmt`, no `shellcheck` findings). If `shfmt -d` shows a diff, run `shfmt -w scripts/changelog-extract.sh` and re-run.

- [ ] **Step 6: Commit**

```bash
git add scripts/changelog-extract.sh
git commit -m "build: add changelog section extraction script"
```

---

## Task 5: Rich release notes in `_publish.yaml`

**Files:**
- Modify: `.github/workflows/_publish.yaml` (the `Create GitHub Release` step — the final step of the `publish` job)

The current step uses `gh release create ... --generate-notes`. Replace it with
a step that builds `release-body.md` in order: curated changelog section →
Helm install/upgrade → image reference → auto-generated notes last. The release
is idempotent (edit-if-exists).

- [ ] **Step 1: Replace the `Create GitHub Release` step**

In `.github/workflows/_publish.yaml`, replace the entire `Create GitHub Release`
step (the last step under `jobs.publish.steps`) with:

```yaml
      - name: Create GitHub Release
        if: inputs.create-github-release
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          RELEASE_TAG: ${{ inputs.release-tag }}
          CHART_VERSION: ${{ inputs.chart-version }}
          IMAGE_VERSION_LABEL: ${{ inputs.image-version-label }}
        run: |
          set -euo pipefail

          cp -f /tmp/bin-amd64/servarr-operator /tmp/servarr-operator-linux-amd64
          cp -f /tmp/bin-arm64/servarr-operator /tmp/servarr-operator-linux-arm64
          cp -f /tmp/bin-darwin-amd64/servarr-operator /tmp/servarr-operator-darwin-amd64
          cp -f /tmp/bin-darwin-arm64/servarr-operator /tmp/servarr-operator-darwin-arm64
          chmod +x \
            /tmp/servarr-operator-linux-amd64 \
            /tmp/servarr-operator-linux-arm64 \
            /tmp/servarr-operator-darwin-amd64 \
            /tmp/servarr-operator-darwin-arm64

          version="${RELEASE_TAG#v}"

          {
            # 1. Curated, user-facing changelog for this version.
            scripts/changelog-extract.sh "${version}" CHANGELOG.md

            # 2. Helm install / upgrade instructions (CRDs first, then operator).
            echo ""
            echo "## Installing / Upgrading"
            echo ""
            echo '```bash'
            echo "# 1. Install/upgrade CRDs + webhook (do this first)"
            echo "helm upgrade --install servarr-crds \\"
            echo "  oci://ghcr.io/phaedrus1992/servarr/servarr-crds --version ${CHART_VERSION}"
            echo ""
            echo "# 2. Install/upgrade the operator"
            echo "helm upgrade --install servarr-operator \\"
            echo "  oci://ghcr.io/phaedrus1992/servarr/servarr-operator --version ${CHART_VERSION}"
            echo '```'

            # 3. Container image reference.
            echo ""
            echo "## Container image"
            echo ""
            echo '```'
            echo "ghcr.io/phaedrus1992/servarr-operator:${RELEASE_TAG}"
            echo '```'
          } > /tmp/release-body.md

          # 4. Auto-generated commit/PR notes, appended at the very end.
          prev_tag="$(git describe --tags --abbrev=0 --match 'v*' "${RELEASE_TAG}^" 2>/dev/null || true)"
          if [ -n "${prev_tag}" ]; then
            gen="$(gh api "repos/${GITHUB_REPOSITORY}/releases/generate-notes" \
              -f tag_name="${RELEASE_TAG}" \
              -f previous_tag_name="${prev_tag}" \
              --jq .body)"
          else
            gen="$(gh api "repos/${GITHUB_REPOSITORY}/releases/generate-notes" \
              -f tag_name="${RELEASE_TAG}" \
              --jq .body)"
          fi
          {
            echo ""
            echo "## Full Changelog"
            echo ""
            printf '%s\n' "${gen}"
          } >> /tmp/release-body.md

          # Idempotent: edit the release in place if the tag was re-pushed.
          if gh release view "${RELEASE_TAG}" >/dev/null 2>&1; then
            gh release edit "${RELEASE_TAG}" --notes-file /tmp/release-body.md --latest
            gh release upload "${RELEASE_TAG}" --clobber \
              /tmp/servarr-operator-linux-amd64 \
              /tmp/servarr-operator-linux-arm64 \
              /tmp/servarr-operator-darwin-amd64 \
              /tmp/servarr-operator-darwin-arm64
          else
            gh release create "${RELEASE_TAG}" \
              --title "${RELEASE_TAG}" \
              --notes-file /tmp/release-body.md \
              --verify-tag \
              --latest \
              /tmp/servarr-operator-linux-amd64 \
              /tmp/servarr-operator-linux-arm64 \
              /tmp/servarr-operator-darwin-amd64 \
              /tmp/servarr-operator-darwin-arm64
          fi
```

Note: the checkout step at the top of this job uses `persist-credentials: false`;
`git describe` only needs local tags (fetched with the checkout), so no extra
permissions are required. `gh api` uses `GH_TOKEN`.

- [ ] **Step 2: Confirm the checkout fetches tags (needed by `git describe`)**

Inspect the `Checkout` step in `.github/workflows/_publish.yaml`. If it does not
set `fetch-depth: 0`, add it so `git describe --tags` can see prior tags:

```yaml
      - name: Checkout
        uses: actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 # v6.0.3
        with:
          persist-credentials: false
          fetch-depth: 0
```

(The `prev_tag` logic already tolerates an empty result on the first release, so
this is belt-and-suspenders for subsequent releases.)

- [ ] **Step 3: Lint the workflow**

Run: `actionlint .github/workflows/_publish.yaml && echo ACTIONLINT_OK`
Expected: `ACTIONLINT_OK` (no findings).

Run: `zizmor .github/workflows/_publish.yaml`
Expected: no new errors versus the pre-change baseline. If `zizmor` flags a
pre-existing issue unrelated to this change, note it but do not expand scope.

- [ ] **Step 4: Render the body locally to verify ordering (no network)**

Simulate the body assembly against the current changelog to confirm section
order and that extraction works. Run:

```bash
RELEASE_TAG=v0.1.0 CHART_VERSION=0.1.0 bash -c '
  set -euo pipefail
  version="${RELEASE_TAG#v}"
  { scripts/changelog-extract.sh "$version" CHANGELOG.md
    echo ""; echo "## Installing / Upgrading"
    echo ""; echo "(helm block here)"
    echo ""; echo "## Container image"
    echo ""; echo "ghcr.io/phaedrus1992/servarr-operator:${RELEASE_TAG}"
    echo ""; echo "## Full Changelog"; echo ""; echo "(generated notes here)"
  }'
```

Expected: prints the Unreleased→0.1.0 changelog body first, then the
Installing/Upgrading, Container image, and Full Changelog headings in that
order. (This proves extraction + ordering; the real `gh api` call runs only in
CI.)

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/_publish.yaml
git commit -m "ci: build rich release notes with changelog and helm instructions"
```

---

## Task 6: Safety rail in `release.yaml`

> **Implementation note (actual):** the Cargo version is read with
> `awk -F'"' '/^version = /{print $2; exit}' Cargo.toml` instead of `grep | sed`
> — deterministic and immune to `grep` output-prefix quirks.

**Files:**
- Modify: `.github/workflows/release.yaml` (the `version` job)

Add a step to the `version` job that fails the release if the tag, the
workspace version, and the changelog disagree. This runs before `publish`
because `publish` already `needs: [version, ci]`.

- [ ] **Step 1: Add a verification step to the `version` job**

In `.github/workflows/release.yaml`, the `version` job currently has a single
step `Compute version from tag`. It needs a checkout (to read `Cargo.toml`,
`CHANGELOG.md`, and the script) before verifying. Replace the `version` job's
`steps:` with:

```yaml
    steps:
      - name: Checkout
        uses: actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 # v6.0.3
        with:
          persist-credentials: false

      - name: Compute version from tag
        id: v
        run: |
          echo "tag=${GITHUB_REF_NAME}" >> "$GITHUB_OUTPUT"
          echo "semver=${GITHUB_REF_NAME#v}" >> "$GITHUB_OUTPUT"

      - name: Verify tag matches Cargo version and changelog
        env:
          SEMVER: ${{ steps.v.outputs.semver }}
        run: |
          set -euo pipefail
          cargo_version="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/')"
          if [ "${cargo_version}" != "${SEMVER}" ]; then
            echo "::error::tag v${SEMVER} does not match [workspace.package] version ${cargo_version}" >&2
            exit 1
          fi
          if ! scripts/changelog-extract.sh "${SEMVER}" CHANGELOG.md >/dev/null; then
            echo "::error::no CHANGELOG.md section for ${SEMVER}; did cargo-release run?" >&2
            exit 1
          fi
          echo "version ${SEMVER}: tag, Cargo.toml, and CHANGELOG.md agree"
```

Note: `grep -m1 '^version = '` reads the first `version = "..."` in the root
`Cargo.toml`, which after Task 1 is the `[workspace.package]` version (the only
`version = "x"` line at column 0; `[workspace.dependencies]` entries are
indented or use `{ version = ... }` inline and are not anchored at line start).

- [ ] **Step 2: Lint the workflow**

Run: `actionlint .github/workflows/release.yaml && echo ACTIONLINT_OK`
Expected: `ACTIONLINT_OK`.

Run: `zizmor .github/workflows/release.yaml`
Expected: no new errors versus baseline.

- [ ] **Step 3: Verify the grep extracts the right version locally**

Run: `grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/'`
Expected: `0.1.0`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yaml
git commit -m "ci: verify tag matches version and changelog before publish"
```

---

## Task 7: `RELEASING.md` runbook

**Files:**
- Create: `RELEASING.md`

- [ ] **Step 1: Create `RELEASING.md`**

Create `RELEASING.md` with exactly:

```markdown
# Releasing

Releases are tag-triggered. `cargo-release` does the local prep; the `v*` tag
fires `.github/workflows/release.yaml`, which builds binaries and publishes the
multi-arch image + Helm charts to GHCR and creates the GitHub Release.

## Steps

1. Make sure `CHANGELOG.md`'s `[Unreleased]` section lists the user-facing
   changes for this release (Keep a Changelog style: Added / Changed / Fixed /
   Removed).

2. Install the tool once:

   ```bash
   cargo install cargo-release
   ```

3. On a release branch, run the version bump. This bumps the shared workspace
   version, rolls `CHANGELOG.md`, bumps both `charts/*/Chart.yaml`, and makes a
   single `chore(release): X.Y.Z` commit. It does NOT tag or push.

   ```bash
   git checkout -b release/x.y.z
   cargo release <patch|minor|major>
   ```

   Add `--execute` to actually apply (cargo-release defaults to a dry run).

4. Open a PR for the release branch and merge it to `main`.

5. Tag the merged commit and push the tag:

   ```bash
   git checkout main && git pull
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   `release.yaml` then verifies the tag matches the version and changelog,
   builds, and publishes.

## Notes

- `cargo-release` is configured with `publish = false`, `tag = false`,
  `push = false` (see `release.toml`) because `main` is protected and there is
  no crates.io publish.
- The GitHub Release body is assembled in CI: the curated `CHANGELOG.md`
  section, Helm install/upgrade instructions, the image reference, and the
  auto-generated commit list last.
```

- [ ] **Step 2: Commit**

```bash
git add RELEASING.md
git commit -m "docs: add release runbook"
```

---

## Task 8: End-to-end `cargo release` dry run

**Files:** none (verification only)

- [ ] **Step 1: Full dry run of a patch release**

Run: `cargo release patch --dry-run 2>&1 | tee /tmp/release-dryrun.txt | tail -40`
Expected: no errors; the output shows it would set version `0.1.1`, update
`CHANGELOG.md`, and update both `Chart.yaml` files. No "Unrendered" or
replacement-count errors.

- [ ] **Step 2: Confirm the dry run mutated nothing**

Run: `git status --short`
Expected: empty output (dry run makes no changes).

- [ ] **Step 3: Confirm changelog rollover would produce a valid section**

Inspect `/tmp/release-dryrun.txt` for the `CHANGELOG.md` replacement preview and
confirm it converts `## [Unreleased]` into `## [0.1.1]` and re-seeds a fresh
`## [Unreleased]` plus a new compare link. If cargo-release does not preview the
replaced content, this is confirmed instead at real release time by the Task 6
safety rail.

- [ ] **Step 4: Push the branch and open the PR**

```bash
git push -u origin feat/release-process
gh pr create --title "Add cargo-release + Keep a Changelog release process" \
  --body "Implements docs/superpowers/specs/2026-06-17-release-process-design.md. Adds CHANGELOG.md, release.toml, RELEASING.md, changelog-extract.sh, shared workspace version, and rich GitHub Release notes (changelog + helm instructions + generated notes). Existing multi-arch image + chart publish pipeline is unchanged." \
  --base main
```

- [ ] **Step 5: Watch CI**

Run: `gh pr checks --watch`
Expected: `actionlint`, `zizmor`, `fmt`, `clippy`, `test`, `helm-lint` all pass.
(`release.yaml` itself only runs on a `v*` tag, so it is not exercised by the PR;
the workflows are validated by `actionlint`/`zizmor` and the local renders in
Tasks 5–6.)

---

## Self-Review Notes

- **Spec coverage:** versioning model (T1), CHANGELOG (T2), release.toml incl.
  chart bumps (T3), changelog-extract.sh + self-check (T4), release-notes
  ordering with generated notes last (T5), safety rail (T6), RELEASING.md (T7),
  verification/dry-run (T8). All spec sections map to a task.
- **No crates.io / no website sync / no git-cliff:** honored — none appear.
- **Type/name consistency:** `scripts/changelog-extract.sh <version>` signature
  is identical in T4 (definition), T5, and T6 (callers). Marker comments
  `<!-- next-header -->` / `<!-- next-url -->` are identical in T2 and T3.
  Chart paths and the `oci://ghcr.io/phaedrus1992/servarr` registry match the
  existing `_publish.yaml`.
