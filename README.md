# Servarr Operator

A Kubernetes operator for declaratively managing media automation applications.
Define your Sonarr, Radarr, Jellyfin, Transmission, and other media apps as
custom resources and let the operator handle deployments, services, storage,
networking, backups, and cross-app integration.

## Supported Applications

| App | Type | Default Port | Tier |
|-----|------|-------------|------|
| Plex | Media server | 32400 | 0 - Media Servers |
| Jellyfin | Media server | 8096 | 0 - Media Servers |
| SABnzbd | Usenet client | 8080 | 1 - Download Clients |
| Transmission | BitTorrent client | 9091 | 1 - Download Clients |
| Sonarr | TV management | 8989 | 2 - Media Managers |
| Radarr | Movie management | 7878 | 2 - Media Managers |
| Lidarr | Music management | 8686 | 2 - Media Managers |
| Tautulli | Plex monitoring | 8181 | 3 - Ancillary |
| Overseerr | Media requests | 5055 | 3 - Ancillary |
| Maintainerr | Media cleanup | 6246 | 3 - Ancillary |
| Prowlarr | Indexer manager | 9696 | 3 - Ancillary |
| Jackett | Indexer proxy | 9117 | 3 - Ancillary |

## Custom Resources

### ServarrApp

Deploy a single application. The operator creates the Deployment, Service,
PersistentVolumeClaims, ConfigMaps, NetworkPolicies, and Gateway API routes.

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  persistence:
    volumes:
      - name: downloads
        mountPath: /downloads
        size: 50Gi
    nfsMounts:
      - name: media
        server: nas.local
        path: /media
        mountPath: /media
        readOnly: true
```

### MediaStack

Deploy a full media stack with shared defaults and tiered rollout. Apps are
brought up in dependency order: media servers first, then download clients,
then media managers, then ancillary services.

```yaml
apiVersion: servarr.dev/v1alpha1
kind: MediaStack
metadata:
  name: media
spec:
  defaults:
    uid: 1000
    gid: 1000
    persistence:
      nfsMounts:
        - name: media
          server: nas.local
          path: /media
          mountPath: /media
          readOnly: true
  apps:
    - app: Jellyfin
      gpu:
        intel: 1
    - app: Transmission
      persistence:
        volumes:
          - name: downloads
            mountPath: /downloads
            size: 100Gi
    - app: Sonarr
    - app: Radarr
    - app: Prowlarr
```

## Features

- **Declarative management** -- define apps as Kubernetes custom resources
- **Tiered rollout** -- MediaStack deploys apps in dependency order
- **Storage** -- PVC volumes and NFS mounts with configurable storage classes
- **Networking** -- Gateway API (HTTPRoute/TCPRoute), TLS via cert-manager, NetworkPolicy generation
- **Backups** -- automated API-driven backups for Servarr v3 apps (Sonarr, Radarr, Lidarr, Prowlarr) with cron scheduling and retention
- **Restore** -- annotation-triggered restore from any backup
- **Cross-app sync** -- Prowlarr automatically discovers and registers Sonarr/Radarr/Lidarr instances
- **Split 4K** -- `split4k: true` on Sonarr/Radarr in a MediaStack automatically creates paired standard and 4K instances
- **Overseerr sync** -- Overseerr automatically discovers and registers Sonarr/Radarr servers with correct 4K flags
- **App configuration** -- Transmission settings.json, SABnzbd host whitelist, Prowlarr custom indexers
- **GPU passthrough** -- NVIDIA, Intel, and AMD device support for hardware transcoding
- **Observability** -- Prometheus metrics and structured JSON logging

## Quick Start

### Prerequisites

- Kubernetes 1.27+
- Helm 3
- cert-manager (optional, for webhook TLS and app TLS certificates)

### Install

```bash
kubectl create namespace servarr

# Install CRDs (requires cluster-admin)
helm install servarr-crds \
  oci://ghcr.io/phaedrus1992/servarr/servarr-crds

# Install operator
helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr
```

To watch all namespaces (requires cluster-admin for the operator):

```bash
helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr \
  --set watchAllNamespaces=true
```

### Deploy an app

```bash
kubectl apply -f - <<EOF
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
EOF
```

### Verify

```bash
kubectl get servarrapp
kubectl get pods
```

## Documentation

- [Installation](docs/installation.md) -- prerequisites, Helm values, upgrade, and uninstall
- [Configuration](docs/configuration.md) -- full CRD field reference
- [Examples](docs/examples.md) -- working YAML for every app type
- [Networking](docs/networking.md) -- services, Gateway API, TLS, and NetworkPolicy
- [Backup and Restore](docs/backup-restore.md) -- API-driven and volume-level backups
- [Admin Credentials](docs/admin-credentials.md) -- declarative admin account management
- [Troubleshooting](docs/troubleshooting.md) -- common issues and diagnosis
- [Contributing](docs/contributing.md) -- development setup and CI commit message flags

## Architecture

The operator is written in Rust and organized as a Cargo workspace:

| Crate | Purpose |
|-------|---------|
| `servarr-crds` | CRD definitions (ServarrApp, MediaStack) |
| `servarr-resources` | Kubernetes resource builders (Deployment, Service, PVC, etc.) |
| `servarr-api` | REST API clients for managed applications |
| `servarr-operator` | Reconciliation controllers, webhook, metrics server |

## License

MIT
