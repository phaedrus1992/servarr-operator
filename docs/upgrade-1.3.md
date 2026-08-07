# Upgrading from 1.2.x to 1.3.x

1.3.0 renamed the operator's Overseerr integration to Seerr (its actively maintained successor)
and added Transmission download-data self-heal. Neither change requires recreating any app, but a
handful of upgrade-time gotchas are easy to hit if you skip straight to `helm upgrade`. This guide
walks through them in the order you're likely to hit them; see the
[CHANGELOG](../CHANGELOG.md) (the `[1.3.0]` entry) for the full list of changes.

## 0. Upgrade CRDs before the operator

As with any release that changes the CRD schema (1.3.0 adds `apiHealthCheck` and the `Seerr` enum
value), upgrade `servarr-crds` first, then the operator:

```bash
helm upgrade servarr-crds oci://ghcr.io/phaedrus1992/servarr/servarr-crds
helm upgrade servarr-operator oci://ghcr.io/phaedrus1992/servarr/servarr-operator --namespace servarr
```

See [Installation: Upgrading](installation.md#upgrading) for the full command reference.

**Version note:** if you're on an operator version older than 1.3.1, upgrade straight to 1.3.1 or
later — 1.3.0 shipped with the reconcile deadlock and CRD-schema issues described below already
fixed on the release branch as of 1.3.1.

## 1. The Seerr rename

`AppType::Overseerr` is renamed to `AppType::Seerr`, and `overseerrSync` to `seerrSync`. Existing
`ServarrApp`/`MediaStack` objects already stored in the cluster keep reconciling with the old
spelling — the operator accepts `app: Overseerr` via a compatibility alias, no forced downtime. As
of 1.3.1, `kubectl apply` of an unmodified pre-1.3 manifest also succeeds (the CRD schema now
accepts the legacy `Overseerr` enum value too). You should still update manifests you actively
maintain, since the alias is a compatibility shim, not a permanent second spelling:

```diff
 spec:
-  app: Overseerr
+  app: Seerr
```

```diff
 spec:
   apps:
     - app: Sonarr
-      overseerrSync:
+      seerrSync:
         enabled: true
```

> **Known gap:** unlike the `app` field, `overseerrSync` doesn't yet have a schema-level alias — a
> manifest that still uses `overseerrSync` is silently pruned by the CRD's structural schema (the
> key is dropped, not rejected) rather than accepted or clearly rejected. Rename it to `seerrSync`
> before re-applying. Tracked in [#545](https://github.com/phaedrus1992/servarr-operator/issues/545).

Renaming a `MediaStack` app entry from `Overseerr` to `Seerr` changes the generated child
`ServarrApp`'s name (e.g. `media-overseerr` → `media-seerr`). Operator versions before 1.3.1 could
wedge the stack's reconcile in a permanent error loop during that transition (the old child was
deleted only *after* trying to apply the new one, which the admission webhook rejected as a
duplicate instance) — fixed in 1.3.1
([#533](https://github.com/phaedrus1992/servarr-operator/issues/533)).

> **This is a name change, not an in-place migration, for `MediaStack` children.** Unlike a
> standalone `ServarrApp` (see below — same name, same PVC, Seerr migrates the inherited database
> in place), a `MediaStack` app rename produces a *new* child name and therefore a *new*,
> empty config PVC (`media-seerr-config`), while the old one (`media-overseerr-config`) is
> preserved but orphaned — 1.3.1 deletes only the old `ServarrApp` object, never its PVC
> (`PropagationPolicy: Orphan`), so your data is never silently destroyed, but it also isn't
> automatically attached to the new Seerr instance. To reuse the old data instead of starting
> Seerr fresh:
>
> ```bash
> # 1. Drop the ownership link left on the orphaned PVC so it isn't cleaned up further.
> kubectl patch pvc media-overseerr-config -n <namespace> --type=json \
>   -p '[{"op":"remove","path":"/metadata/ownerReferences"}]'
> # 2. Point the new Seerr entry's config volume at the old claim (existingClaimName skips
> #    PVC creation and binds to the existing one) before applying the renamed MediaStack.
> ```
> ```yaml
> apps:
>   - app: Seerr
>     persistence:
>       volumes:
>         - name: config
>           existingClaimName: media-overseerr-config
> ```

Seerr's image and config volume also change on the same transition:

- **Image:** `linuxserver/overseerr:1.35.0` → `ghcr.io/seerr-team/seerr:v3.4.1` (repository moved).
  Seerr detects an inherited Overseerr database on first boot and migrates it in place — no manual
  data export/import needed.
- **Config mount path:** `/config` → `/app/config`.
- **User:** fixed uid/gid `1000:1000` (Seerr's image runs as a non-configurable `node` user, unlike
  the LinuxServer image it replaces, which honored your stack's PUID/PGID). As of 1.3.1, the
  operator auto-migrates the inherited config volume's ownership to `1000:1000` on first reconcile
  after the transition (an init container conditionally chowns it — see
  [#535](https://github.com/phaedrus1992/servarr-operator/issues/535)); on 1.3.0 this required a
  manual `chown -R 1000:1000` on the volume.

As with any operator-managed change that replaces a running deployment, **take a PVC snapshot of
the app's `config` volume before upgrading** an existing Overseerr app (see
[Volume-Level Backups with Velero](backup-restore.md#volume-level-backups-with-velero)).

### Stale `defaultImages.overseerr` Helm value

If you `helm upgrade --reuse-values`, a previously-set `defaultImages.overseerr` value persists
in the release's computed values even though the 1.3.0 chart renamed that key to
`defaultImages.seerr`. The operator still prefers an explicit `DEFAULT_IMAGE_SEERR_*` value over
the legacy `DEFAULT_IMAGE_OVERSEERR_*` fallback, so this doesn't silently pin the old image — but
as of 1.3.1 the fallback firing at all (e.g. because you never set `defaultImages.seerr`
explicitly) also publishes a `DeprecatedImageOverride` Warning Event on the affected app
(`kubectl get events`), not just a startup log line
([#534](https://github.com/phaedrus1992/servarr-operator/issues/534)). Rename the value to avoid
it entirely:

```diff
 defaultImages:
-  overseerr:
+  seerr:
     repository: ghcr.io/seerr-team/seerr
     tag: v3.4.1
```

## 2. Remove any legacy `appConfig.transmission.auth` block

If a `Transmission` app has both `spec.adminCredentials` and the older
`spec.appConfig.transmission.auth` block set, the generated Deployment ends up with duplicate
`USER`/`PASS` env vars and the server-side apply is rejected
([#536](https://github.com/phaedrus1992/servarr-operator/issues/536)). Remove the legacy block —
`adminCredentials` already covers the same credentials:

```diff
 spec:
   adminCredentials:
     secretName: transmission-auth
   appConfig:
     transmission:
-      auth:
-        secretName: transmission-auth
+      # auth removed -- superseded by spec.adminCredentials above
```

## 3. `apiKeySecret` is required for API-level health checks

Enabling `apiHealthCheck.enabled: true` on a Servarr-family app (Sonarr, Radarr, Lidarr, Prowlarr),
Seerr, or SABnzbd requires `spec.apiKeySecret` — without it, the `ApiKeyReadError` status condition
goes `Unknown` and the API probe is skipped (falls back to a plain HTTP probe). Transmission is the
exception: it uses `adminCredentials` instead, no `apiKeySecret` needed. Jellyfin and Plex are
probed anonymously. See [`apiKeySecret`](configuration.md#apikeysecret) and
[`apiHealthCheck`](configuration.md#apihealthcheck).

## 4. Transmission download-data self-heal (new, opt-in)

1.3.0 adds detection (and, opt-in, removal) of Transmission torrents whose on-disk data has gone
missing. Detection alone needs `apiHealthCheck.enabled: true`; actually removing a confirmed-orphan
torrent additionally needs `apiHealthCheck.autoRemoveOrphanedTorrents: true` — a separate,
deliberately-named opt-in so upgrading never starts deleting torrent entries without explicit
consent. Only Transmission's bookkeeping entry is ever removed; on-disk files are never touched by
this feature. See [Transmission download-client self-heal](configuration.md#apihealthcheck) for the
full behavior and batch-size limits.

## Summary checklist

- [ ] Upgrade `servarr-crds`, then `servarr-operator` (in that order)
- [ ] If not already on 1.3.1+, plan to upgrade there next — several of the fixes below shipped
      after 1.3.0
- [ ] Rename `app: Overseerr` → `app: Seerr` and `overseerrSync` → `seerrSync` in manifests you
      maintain
- [ ] Rename `defaultImages.overseerr` → `defaultImages.seerr` in your Helm values
- [ ] Take a PVC snapshot of any Overseerr app's `config` volume before changing `spec.app`
- [ ] For a `MediaStack` app rename specifically: reattach the old (orphaned, not deleted)
      config PVC via `existingClaimName` if you want Seerr to inherit its data
- [ ] Remove any legacy `appConfig.transmission.auth` block that duplicates `adminCredentials`
- [ ] Set `apiKeySecret` on any app newly opting into `apiHealthCheck`
- [ ] Review whether Transmission `autoRemoveOrphanedTorrents` fits your setup before enabling it
