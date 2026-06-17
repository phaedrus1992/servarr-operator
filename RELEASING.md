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
- The CHANGELOG rollover and `Chart.yaml` version bumps are configured on the
  `servarr-operator` crate's `[package.metadata.release]` (this is a virtual
  workspace, so the replacements are anchored to a single crate with `../../`
  paths and run exactly once).
- The GitHub Release body is assembled in CI: the curated `CHANGELOG.md`
  section, Helm install/upgrade instructions, the image reference, and the
  auto-generated commit list last.
