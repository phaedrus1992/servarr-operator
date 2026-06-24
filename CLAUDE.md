# servarr-operator — Project Rules

## Versioning

**Never manually edit the version in `Cargo.toml`.**
Version bumps are managed exclusively by `cargo-release`. Running `cargo release patch|minor|major`
handles the bump, tag, and publish in one step. A hand-edit will conflict with `cargo-release`'s
own diff detection and may produce a double-bump or a mismatch between the tag and the
`Cargo.toml` at HEAD.

CHANGELOG entries are still written by hand (or the `keepachangelog` skill) — only the version
number in `Cargo.toml` / `Cargo.lock` is off-limits for direct edits.

### Changelog: default image bumps are user-facing

The generic Keep a Changelog guidance to omit dependency bumps does **not** apply to the default
application images in `image-defaults.toml`. Those pins are what users actually run, so every bump
gets a `Changed` entry naming the app, old → new tag, and (for repository moves) the new path.

For a major upstream bump, fetch the upstream release notes and summarize the user-relevant
highlights — new features, behavior changes, and especially any non-backward-compatible migration
the user must be aware of. Patch/rolling bumps (e.g. Jackett indexer-definition rollups) need only
a one-line entry. CI/GitHub-Actions and Rust crate dependency bumps stay omitted unless they change
operator behavior.

## Release Branch Workflow

Issues are milestoned by category, not version. The milestone determines the base branch:

- **Bug Fixes** and **Small Enhancements** → branch from and target the newest `release/N.x`
  line (currently `release/1.x`). These ship in patch/minor releases off that line.
- **Large Features** → branch from and target `main`.

The "newest `release/N.x`" is the highest-major release line on the remote (`release/1.x` today,
`release/2.x` once it exists). Branches are long-lived per major series, not per patch — there is
no `release/1.0.x`.

Note: dev-sprint's `resolve-base-branch.sh` only auto-resolves a base when the milestone title
carries a version token (e.g. `1.0`). Category milestones have none, so it falls back to `main`.
For category milestones, pick the base by the rule above — do not trust the auto-resolver.

Never retarget a milestone-scoped PR to a different base without explicit user approval.

## CI Toolchain Note

CI runs Rust 1.94.0, which may enforce stricter Clippy lints than the local toolchain. Always run
`cargo clippy --all-targets --all-features -- -D warnings` locally before pushing to catch
lint regressions early. Known stricter lints on 1.94: `clippy::bool_comparison`.
