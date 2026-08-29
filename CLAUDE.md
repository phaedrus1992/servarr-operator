# servarr-operator — Project Rules

## Versioning

**Never manually edit the version in `Cargo.toml`.**
Version bumps are managed exclusively by `cargo-release`. Running `cargo release patch|minor|major`
handles the bump, tag, and publish in one step. A hand-edit will conflict with `cargo-release`'s
own diff detection and may produce a double-bump or a mismatch between the tag and the
`Cargo.toml` at HEAD.

CHANGELOG entries are still written by hand (or the `keepachangelog` skill) — only the version
number in `Cargo.toml` / `Cargo.lock` is off-limits for direct edits.

### Changelog: dedicated "Image Updates" section per version

The generic Keep a Changelog guidance to omit dependency bumps does **not** apply to the default
application images in `image-defaults.toml`. Those pins are what users actually run, so every bump
is recorded — but not folded into the generic `Changed` bullets. Instead, every version entry that
touched `image-defaults.toml` gets its own `### Image Updates` subsection, positioned after
`Changed` and before `Fixed`. **Omit the whole section entirely on a version with no image
changes** — don't emit an empty header.

**Per-entry format**, one line per app:

```markdown
### Image Updates

- **<App>**: `<old-repo:old-tag>` -> `<new-repo:new-tag>`
```

Drop the repo path from the tag when it didn't change (`` `4.1.2` -> `4.1.3` ``); include the full
`repo:tag` on both sides when the repository moved.

**Release-note highlights.** When the bump crosses a minor/major upstream version (not a
patch/rolling release), fetch that project's own release notes for every version between the old
and new tag and add an indented bullet list beneath its line — new features, behavior changes, and
especially anything not backward compatible that the user needs to act on:

```markdown
- **Seerr**: `linuxserver/overseerr:1.35.0` -> `ghcr.io/seerr-team/seerr:v3.4.1` (repository moved)
  - Merged with Jellyseerr; actively maintained successor to the now-archived Overseerr
  - Runs as a fixed UID/GID `1000` (not configurable via PUID/PGID like the LinuxServer image)
  - Auto-migrates an inherited Overseerr database in-place on first boot
```

Patch/rolling bumps (e.g. Jackett indexer-definition rollups) get just the version-change line, no
highlights sublist. CI/GitHub-Actions and Rust crate dependency bumps still stay omitted from the
changelog entirely unless they change operator behavior — this section is for `image-defaults.toml`
only.

**Ordering within the section:** list apps in the order their bumps landed (git log order), not
alphabetically — matches how the rest of this changelog reads chronologically within a version.

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

CI runs Rust 1.97.1, which may enforce stricter Clippy lints than the local toolchain. Always run
`cargo clippy --all-targets --all-features -- -D warnings` locally before pushing to catch
lint regressions early. Known stricter lints on 1.94: `clippy::bool_comparison`. The 1.94 -> 1.97.1
bump (v1.3.1) surfaced no additional stricter lints against this codebase.

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
