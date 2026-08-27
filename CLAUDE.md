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

Work targeting milestone `X.Y` branches from and targets `release/X.Y.x`, not `main`. The
`resolve-base-branch.sh` script in dev-sprint determines the correct base automatically. Never
retarget a milestone-scoped PR to `main` without explicit user approval.

## CI Toolchain Note

CI runs Rust 1.97.1, which may enforce stricter Clippy lints than the local toolchain. Always run
`cargo clippy --all-targets --all-features -- -D warnings` locally before pushing to catch
lint regressions early. Known stricter lints on 1.94: `clippy::bool_comparison`. The 1.94 -> 1.97.1
bump (v1.3.1) surfaced no additional stricter lints against this codebase.
