# NFS Volumes and Media Storage

The MediaStack operator can deploy an in-cluster NFS server and automatically
inject the right storage mounts into every app.  This page covers how the
feature works, how to configure it, and common deployment patterns.

---

## How it works

When a `MediaStack` has an `nfs:` spec (or when the field is omitted and the
cluster default is used), the operator:

1. **Deploys an NFS server** — a single-replica StatefulSet backed by a PVC,
   running [itsthenetwork/nfs-server-alpine](https://github.com/sjiveson/nfs-server-alpine).
   The server exports `/nfsshare` over port 2049 and is reachable at
   `<stack-name>-nfs-server.<namespace>.svc.cluster.local`.

2. **Auto-injects NFS mounts** into each app's pod spec based on app type:

| App | Injected mounts (container path → NFS path) |
|-----|---------------------------------------------|
| Plex, Jellyfin | `/movies`, `/tv`, `/music`, `/movies-4k`, `/tv-4k` |
| Sonarr (standard) | `/tv` → `/nfsshare/tv` |
| Sonarr (4K) | `/tv` → `/nfsshare/tv-4k` |
| Radarr (standard) | `/movies` → `/nfsshare/movies` |
| Radarr (4K) | `/movies` → `/nfsshare/movies-4k` |
| Lidarr | `/music` → `/nfsshare/music` |
| SABnzbd, Transmission | `/movies`, `/tv`, `/music`, `/movies-4k`, `/tv-4k` |
| All other apps | — (no automatic mounts) |

Mounts are injected read-write.  Apps not listed above (Prowlarr, Overseerr,
Maintainerr, etc.) receive no automatic mounts; add them explicitly via
`persistence.nfsMounts` if needed.

The 4K Sonarr and Radarr instances present the same container path as the
standard instances (`/tv`, `/movies`), so no special app configuration is
required.  Only the NFS server-side path differs, keeping both instances on
separate directory trees.

---

## Default configuration

Omitting the `nfs:` field is equivalent to:

```yaml
nfs:
  enabled: true
  storageSize: 1Ti
  moviesPath: /movies
  tvPath: /tv
  musicPath: /music
  movies4kPath: /movies-4k
  tv4kPath: /tv-4k
```

No `nfs:` block is required for a working stack.

---

## Configuration reference

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `true` | Deploy the in-cluster NFS server. Set `false` to disable NFS entirely. |
| `storageSize` | `1Ti` | Size of the backing PVC. |
| `storageClass` | cluster default | Storage class for the PVC. |
| `image` | built-in | Override the NFS server container image. |
| `moviesPath` | `/movies` | Media subpath for movies. Used as the container mount path and appended to the NFS export root. |
| `tvPath` | `/tv` | Media subpath for TV shows. |
| `musicPath` | `/music` | Media subpath for music. |
| `movies4kPath` | `/movies-4k` | Media subpath for 4K movies (used by split4k Radarr). |
| `tv4kPath` | `/tv-4k` | Media subpath for 4K TV (used by split4k Sonarr). |
| `externalServer` | — | Address of an external NFS server. Disables the in-cluster server. |
| `externalPath` | `/` | Root export path on the external server, prepended to all media subpaths. |

---

## Deployment patterns

### Minimal — in-cluster NFS with default paths

No `nfs:` block needed.  The operator sizes the PVC at 1 Ti using the cluster
default storage class.

```yaml
apiVersion: servarr.dev/v1alpha1
kind: MediaStack
metadata:
  name: media
spec:
  apps:
    - app: Sonarr
    - app: Radarr
    - app: SABnzbd
    - app: Plex
```

### In-cluster NFS with explicit PVC configuration

```yaml
nfs:
  storageSize: 4Ti
  storageClass: longhorn
```

### In-cluster NFS backed by host-local storage

Use a `local-path` or `local` storage class to provision the PVC from a
specific node's disk:

```yaml
nfs:
  storageSize: 8Ti
  storageClass: local-path    # e.g. Rancher local-path-provisioner
```

Because `local-path` PVCs are node-bound, the NFS server pod will always
schedule to the same node.  All other pods reach it over the cluster network
via the ClusterIP Service.

### External NAS

Disable the in-cluster server and point every app at your NAS instead:

```yaml
nfs:
  externalServer: nas.home.arpa
  externalPath: /volume1    # /volume1/movies, /volume1/tv, …
```

The operator still auto-injects the mounts into each app; it just uses the
external address and the `externalPath` root instead of deploying a server.

### Custom media paths

Override any or all of the default subpaths:

```yaml
nfs:
  moviesPath: /Media/Movies
  tvPath: /Media/TV
  musicPath: /Media/Music
  movies4kPath: /Media/Movies-4K
  tv4kPath: /Media/TV-4K
```

### Disable NFS entirely

Opt out of automatic NFS management and handle volumes manually:

```yaml
nfs:
  enabled: false
```

With NFS disabled, no mounts are injected.  Configure `persistence.nfsMounts`
per app (or via `defaults.persistence.nfsMounts`) as needed.

---

## Split 4K

Setting `split4k: true` on a Sonarr or Radarr app creates two child
ServarrApp instances: `<stack>-sonarr` and `<stack>-sonarr-4k`.

Both instances receive the **same container mount path** (`/tv` or `/movies`)
so the Sonarr/Radarr configuration is identical.  The operator routes each
instance to a separate NFS directory on the server side:

| Instance | Container path | NFS server path |
|----------|----------------|-----------------|
| `media-sonarr` | `/tv` | `/nfsshare/tv` |
| `media-sonarr-4k` | `/tv` | `/nfsshare/tv-4k` |
| `media-radarr` | `/movies` | `/nfsshare/movies` |
| `media-radarr-4k` | `/movies` | `/nfsshare/movies-4k` |

This lets Overseerr target the standard and 4K instances independently without
any special per-instance Sonarr/Radarr configuration.

To use different mount paths for the 4K instances, add explicit overrides via
`split4kOverrides.persistence`:

```yaml
- app: Sonarr
  split4k: true
  split4kOverrides:
    persistence:
      nfsMounts:
        - name: tv
          server: media-nfs-server.media.svc.cluster.local
          path: /nfsshare/tv-4k
          mountPath: /tv-4k    # different container path for 4K
```

---

## Apps that need explicit mounts

Apps outside the auto-injection list (Maintainerr, SshBastion, etc.) need
explicit `persistence.nfsMounts` entries.  Use the in-cluster server's DNS
name: `<stack-name>-nfs-server.<namespace>.svc.cluster.local`.

```yaml
- app: SshBastion
  persistence:
    nfsMounts:
      - name: movies
        server: media-nfs-server.media.svc.cluster.local
        path: /nfsshare/movies
        mountPath: /movies
      - name: tv
        server: media-nfs-server.media.svc.cluster.local
        path: /nfsshare/tv
        mountPath: /tv
```

For an external NFS server, substitute the server address and the appropriate
path prefix (`<externalPath><mediaPath>`).
