# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

<!-- next-header -->

## [Unreleased] - ReleaseDate

### Fixed

- Fix default liveness probe (`timeout_seconds: 1`, `failure_threshold: 3`) being too
  aggressive for .NET-based *arr apps (Sonarr, Radarr, Lidarr). Brief HTTP unresponsiveness
  during RSS syncs, library scans, or GC pauses could trip 3 consecutive 1s-timeout failures
  in 30s and get the pod SIGKILLed even though it was healthy. Raised to `timeout_seconds: 5`,
  `failure_threshold: 5` (~50s grace). Override via `probes:` on the ServarrApp CR if you need
  different values (#173).
- Fix Maintainerr sync silently masking Sonarr/Radarr API list failures as "no servers
  registered", which caused duplicate server registrations on retry instead of a visible
  reconcile error. The operator now propagates these API errors so the controller retries
  with backoff instead of silently proceeding with a stale empty list (#199).
- Fix Maintainerr `DATA_DIR` not being wired to the config volume mount path. The operator
  now auto-injects `DATA_DIR` for Maintainerr equal to the `config` volume's `mountPath`
  (defaulting to `/opt/data`). Previously, users who mounted their PVC at `/config`
  (following the convention for other apps) would see a fresh empty database on every
  restart because Maintainerr reads from `DATA_DIR`, not `/config`. **Migration:** if your
  CR has `mountPath: /config` for the Maintainerr config volume, change it to
  `mountPath: /opt/data`; the files on the PVC do not need to move. You can also keep
  `mountPath: /config` and add `DATA_DIR: /config` to `spec.env` as an alternative.

### Changed

- Update default Sonarr image to `4.0.18` (from `4.0.17`).
- Update default Jackett image to `0.24.2126` (from `0.24.2116`), rolling up upstream
  indexer-definition updates.

## [1.1.1] - 2026-07-01

### Changed

- Update default Radarr image to `6.1.1` (from `6.0.4`).
- Update default Tautulli image to `2.17.1` (from `2.16.1`).
- Update default Jackett image to `0.24.2116` (from `0.24.2075`), rolling up upstream
  indexer-definition updates.
- Update default Subgen image to `2026.06.3` (from `2026.04.3`).

## [1.1.0] - 2026-06-24

### Added

- Auto-configure Maintainerr. When a Maintainerr `ServarrApp` sets
  `maintainerrSync.enabled`, the operator registers the namespace's Sonarr, Radarr,
  Overseerr, and Tautulli instances into Maintainerr (including split4k variants),
  replacing the manual API workaround. Registration is idempotent. Adds the
  `maintainerrSync` spec field and a `MaintainerrSyncReady` status condition.
  (Plex sync is not yet supported — it needs a plex.tv token source, tracked in #148.)

### Changed

- Update default Maintainerr image to `3.15.2` (from `2.19.0`) and move its repository
  from `ghcr.io/jorenn92/maintainerr` to `ghcr.io/maintainerr/maintainerr`. Upstream v3
  adds Jellyfin support, unifies Overseerr/Jellyseerr handling, and reports collection
  sizes on the dashboard. **Note:** v3's database schema is not backward compatible with
  2.x — existing Maintainerr data is migrated forward on first start and cannot be rolled
  back. Combined with the new `/opt/data` path and 2Gi memory default (see Fixed).
- Update default Jackett image to `0.24.2075` (from `0.24.2057`), rolling up upstream
  indexer-definition updates.

### Fixed

- Fix Maintainerr default data path and memory. Maintainerr v3 stores data at `/opt/data`
  (not `/config`), and large library scans need more headroom — the memory limit is raised
  from 512Mi to 2Gi (#131, #138).
- Fix Subgen running out of memory during transcription. The Whisper `medium` model needs
  2Gi; the default memory limit is raised from 512Mi to 2Gi.
- Fix SSH bastion `restricted-rsync` wrapper rejecting paths containing parentheses
  (e.g. `Show (2024)/`). rsync always escapes parentheses as `\(` and `\)` in the
  remote command; the metacharacter guard now uses an ERE check to distinguish
  rsync-escaped parens from bare subshell injection attempts (e.g. `$(id)` or `(id)`).

## [1.0.3] - 2026-06-21

### Fixed

- Fix SSH bastion pod not restarting when `authorized-keys` Secret or `restricted-rsync`
  ConfigMap changes. The `config_checksum` pod-annotation hash previously covered only the
  main app ConfigMap and Prowlarr definitions; it now also hashes the `authorized-keys`
  Secret string data and the `restricted-rsync` ConfigMap so rotating SSH keys or updating
  the wrapper script triggers a rolling restart automatically.
- Fix SSH bastion `restricted-rsync` wrapper rejecting real rsync server-mode combined
  flags (e.g. `-vlogDtprze.iLsfxCIvu`). The flag allowlist regex `[^vzrltpgo]` was too
  narrow for the combined short flags rsync uses in practice. The allowlist is removed;
  `--sender` already enforces read-only at the protocol level, matching `rrsync`'s approach.

## [1.0.2] - 2026-06-21

### Fixed

- Fix SSH bastion `authorized_keys` containing broken symlinks. The `copy-authorized-keys`
  init container copied Kubernetes Secret-mount symlinks as-is; it now dereferences each key
  file so `sshd` can read them.
- Fix container image tags and Helm chart `appVersion` carrying a `v` prefix. They now use
  bare semver (`1.0.2`, not `v1.0.2`) so source charts, deployed `appVersion`, and image tags
  all agree.
- Fix SSH bastion restricted-rsync wrapper dropping audit log entries silently when syslog
  is unavailable in the container. Rejected and allowed rsync events now fall back to stderr
  so they appear in `kubectl logs` even without a syslog socket.
- Fix SSH bastion admission webhook accepting `user.shell` values that are non-absolute or
  contain colons or shell metacharacters. A colon would corrupt the colon-delimited
  `SSH_USERS` env var format; the webhook now rejects such values at admission time.
- Fix SSH bastion admission webhook accepting user names and `allowedPaths` values
  containing shell metacharacters. User names are now validated against
  `^[a-z_][a-z0-9_-]{0,31}$`; allowed paths must be absolute and must not contain
  `"`, `\`, `$`, backtick, or whitespace. Invalid values are rejected at admission
  time with a descriptive error.
- Fix restricted-rsync wrapper permitting arbitrary rsync flags such as `--log-file`.
  Only a known-safe flag set (`--server`, `--sender`, `--numeric-ids`, `--timeout`,
  `-e*`, and short flags `vzrltpgo`) is now allowed; unrecognized flags and bare-word
  arguments before the path separator are rejected.
- Fix SSH bastion restricted-rsync rejecting paths with spaces and not expanding globs. The
  wrapper kept only the last whitespace-separated token of the source path (so
  `/media/Show Name/` became `Name/` and was rejected) and passed globs to `rsync` unexpanded.
  It now parses the command like a login shell — rejecting injection-prone metacharacters,
  then word-splitting and glob-expanding — and validates every source path against the
  allowlist.

## [1.0.1] - 2026-06-18

### Changed

- Raise default memory for download clients (SABnzbd, Transmission, Sonarr, Radarr, Lidarr)
  from 512Mi limit / 128Mi request to 1Gi limit / 256Mi request. Indexer-only apps (Prowlarr)
  keep the lower default.

### Fixed

- Fix SSH bastion `authorized_keys` rejected by `sshd StrictModes`. Kubernetes Secret mounts
  use world-writable tmpfs directories that StrictModes unconditionally rejects. A new
  `copy-authorized-keys` init container copies the Secret to an `emptyDir` volume with correct
  permissions (`chmod 700` on the directory, `chmod 644` on key files, `chown root:root`).
  The init container is only added when at least one user has public keys configured.
- Fix webhook rejecting valid SSH bastion gateway configs. The validation previously required
  `gateway.hosts` to be non-empty for all route types; SSH bastion always uses `TCPRoute`,
  which has no `hostname` field and must have an empty hosts list.
- Fix webhook silently accepting `gateway.hosts` on TCP routes. Non-empty hosts are now
  rejected with an error message explaining that `TCPRoute` discards hostname configuration.

## [1.0.0] - 2026-06-18

Initial public release. The operator declaratively manages media automation
applications on Kubernetes through two custom resources and handles the full
lifecycle: deployment, storage, networking, backups, and cross-app integration.

### Added

- Add the `ServarrApp` custom resource for deploying a single application. The
  operator reconciles a Deployment, Service, PersistentVolumeClaims, ConfigMaps,
  NetworkPolicies, and Gateway API routes from one spec.
- Add the `MediaStack` custom resource for deploying a full stack with shared
  defaults and tiered rollout (media servers, then download clients, then media
  managers, then ancillary services), with per-app override and orphan cleanup.
- Support 15 applications across 4 tiers: Plex, Jellyfin, SshBastion, SABnzbd,
  Transmission, Sonarr, Radarr, Lidarr, Tautulli, Overseerr, Maintainerr,
  Prowlarr, Jackett, Bazarr, and Subgen, each with built-in image, port,
  security profile, probe, and volume defaults.
- Add image resolution with field-level inheritance: pin only `image.tag` (or
  any single sub-field) and the rest fall back to the per-app default. The same
  inheritance applies to `DEFAULT_IMAGE_<APP>_*` operator overrides.
- Add three security profiles -- `LinuxServer` (s6-overlay), `NonRoot`, and
  `Custom` -- controlling capabilities, run-as user/group, and fsGroup.
- Add storage support: PVC-backed volumes (with `existingClaimName` to adopt
  pre-existing claims), inline NFS mounts, and configurable storage classes.
- Add an in-cluster NFS server for MediaStack that auto-injects per-app media
  mounts, with an option to point at an external NAS instead.
- Add networking: ClusterIP/NodePort/LoadBalancer services, host-port binding
  (with automatic Recreate strategy), Gateway API HTTPRoute/TCPRoute, TLS via
  cert-manager, and NetworkPolicy generation (ingress + egress, denied CIDR
  ranges, gateway-namespace auto-allow, Transmission peer-port ingress).
- Add a `serviceName` override to preserve stable Service DNS names.
- Add `split4k` on Sonarr/Radarr in a MediaStack to create paired standard and
  4K instances on separate storage paths, with per-instance overrides.
- Add API-driven backups for Servarr v3 apps (Sonarr, Radarr, Lidarr, Prowlarr)
  with cron scheduling and retention, plus annotation-triggered restore and
  Velero volume-exclusion annotations.
- Add declarative admin-credential management via referenced Secrets, applied
  through env injection (Servarr v3) or live API calls (SABnzbd, Transmission,
  Jellyfin, Tautulli, Overseerr, Bazarr) and re-applied on Secret rotation.
- Add cross-app synchronization: Prowlarr registers Sonarr/Radarr/Lidarr,
  Overseerr registers Sonarr/Radarr with correct 4K flags, Bazarr registers
  Sonarr/Radarr for subtitles, and Subgen wires up to a Jellyfin instance.
- Add app-specific configuration: Transmission settings/peer-port/auth, SABnzbd
  host whitelist and tar unpacking, Prowlarr custom indexer definitions, and an
  SSH bastion with per-user access modes (shell, sftp, scp, rsync,
  restricted-rsync).
- Add GPU passthrough for NVIDIA, Intel, and AMD devices, plus Node Feature
  Discovery-based scheduling for hardware transcoding.
- Add a validating admission webhook enforcing port ranges, resource limits,
  unique volume/mount names, immutable app/instance, and app-config consistency.
- Add drift detection that reconciles live Deployment drift back to spec, API
  health checks, and update-available conditions for Servarr v3 apps.
- Add observability: Prometheus metrics for reconciles, drift corrections,
  backups, and managed apps/stacks, plus structured JSON logging.
- Add namespace-scoped and cluster-wide (`watchAllNamespaces`) operation, a
  Secret watcher for timely credential rotation, and a `crd` subcommand that
  prints CRD YAML.
- Add release automation: `cargo-release` + Keep a Changelog, with the
  multi-arch container image and Helm charts (CRDs + operator) published to GHCR
  on each `v*` tag.

<!-- next-url -->
[Unreleased]: https://github.com/phaedrus1992/servarr-operator/compare/v1.1.1...HEAD
[1.1.1]: https://github.com/phaedrus1992/servarr-operator/compare/v1.1.0...v1.1.1
[1.1.0]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.3...v1.1.0
[1.0.3]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/phaedrus1992/servarr-operator/compare/50a4a1eb98121d552a37ba8dcf6f38043478d8d5...v1.0.0
