# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

<!-- next-header -->

## [Unreleased] - ReleaseDate

### Fixed

- Fix SSH bastion `authorized_keys` containing broken symlinks. The `copy-authorized-keys`
  init container copied Kubernetes Secret-mount symlinks as-is; it now dereferences each key
  file so `sshd` can read them.
- Fix container image tags and Helm chart `appVersion` carrying a `v` prefix. They now use
  bare semver (`1.0.2`, not `v1.0.2`) so source charts, deployed `appVersion`, and image tags
  all agree.
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
[Unreleased]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.1...HEAD
[1.0.1]: https://github.com/phaedrus1992/servarr-operator/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/phaedrus1992/servarr-operator/compare/50a4a1eb98121d552a37ba8dcf6f38043478d8d5...v1.0.0
