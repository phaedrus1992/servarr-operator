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

### Changelog: group all app image updates together

Within the `Changed` section, keep all "Update default <app> image" entries grouped together
in a single contiguous block. Non-image entries (feature changes, behavior changes, config
deprecations) go before or after the image block, never interleaved between individual image
bumps. This prevents a single image update from being visually buried between unrelated entries
and makes the image sweep easy to scan at a glance.

## Release Branch Workflow

Work targeting milestone `X.Y` branches from and targets `release/X.Y.x`, not `main`. The
`resolve-base-branch.sh` script in dev-sprint determines the correct base automatically. Never
retarget a milestone-scoped PR to `main` without explicit user approval.

## CI Toolchain Note

CI runs Rust 1.94.0, which may enforce stricter Clippy lints than the local toolchain. Always run
`cargo clippy --all-targets --all-features -- -D warnings` locally before pushing to catch
lint regressions early. Known stricter lints on 1.94: `clippy::bool_comparison`.
