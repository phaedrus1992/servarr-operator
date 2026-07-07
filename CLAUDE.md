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

## Module Size

Keep production code (everything outside `#[cfg(test)] mod tests`) under **~500 lines per file**;
treat **~800 lines** as a hard signal to split by concern into submodules, regardless of test code
appended below it. A file holding more than ~15 top-level functions is the same signal in
function-count form — group related functions (e.g. backup/restore, admin-credential sync, status
reporting, cross-app sync) into their own modules under a directory named for the parent (e.g.
`controller/backup.rs`, `controller/status.rs`) rather than adding another function to an
already-large file.

Test code naturally grows large (`#[cfg(test)] mod tests` blocks are exempt from this limit) — the
limit targets production logic, where file size is a proxy for how many unrelated concerns got
bolted onto one module over time. When adding a new function to a file already past ~500 production
lines, prefer creating or extending a submodule over appending to the existing file, unless the new
function is tightly coupled to existing code in that file (shares private helpers, same struct
impl block, etc.).
