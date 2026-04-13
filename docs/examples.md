# ServarrApp Examples

Each app has a standalone YAML file in [`examples/`](examples/) that can be
applied directly with `kubectl apply -f`. The operator fills in sensible
defaults for image, service, probes, security context, and persistence when
fields are omitted.

Files are listed in rollout tier order (the order the operator brings apps up
in a MediaStack).

## Tier 0 -- Media Servers

| App | File | Notes |
|-----|------|-------|
| Plex | [`plex.yaml`](examples/plex.yaml) | GPU transcoding variants included |
| Jellyfin | [`jellyfin.yaml`](examples/jellyfin.yaml) | Intel, NVIDIA, AMD GPU variants |
| SshBastion | [`ssh-bastion.yaml`](examples/ssh-bastion.yaml) | SSH jump host |

## Tier 1 -- Download Clients

| App | File | Notes |
|-----|------|-------|
| SABnzbd | [`sabnzbd.yaml`](examples/sabnzbd.yaml) | Reverse proxy whitelist, tar unpacking |
| Transmission | [`transmission.yaml`](examples/transmission.yaml) | Auth, peer port, settings override |

## Tier 2 -- Media Managers

| App | File | Notes |
|-----|------|-------|
| Sonarr | [`sonarr.yaml`](examples/sonarr.yaml) | Multi-instance, API backups |
| Radarr | [`radarr.yaml`](examples/radarr.yaml) | |
| Lidarr | [`lidarr.yaml`](examples/lidarr.yaml) | |

## Tier 3 -- Ancillary

| App | File | Notes |
|-----|------|-------|
| Tautulli | [`tautulli.yaml`](examples/tautulli.yaml) | Plex monitoring |
| Overseerr | [`overseerr.yaml`](examples/overseerr.yaml) | Media requests |
| Maintainerr | [`maintainerr.yaml`](examples/maintainerr.yaml) | Nonroot security profile |
| Prowlarr | [`prowlarr.yaml`](examples/prowlarr.yaml) | Cross-app sync, custom indexers |
| Jackett | [`jackett.yaml`](examples/jackett.yaml) | |

## Backup Configuration

Sonarr, Radarr, Lidarr, and Prowlarr support API-driven backups. See
[`sonarr.yaml`](examples/sonarr.yaml) for a backup example, and
[Backup and Restore](backup-restore.md) for full documentation.

Create the API key secret:

```bash
kubectl create secret generic sonarr-api-key \
  --from-literal=api-key=your-api-key-here
```

The same pattern works for Radarr, Lidarr, and Prowlarr -- replace the app
type, name, and secret reference accordingly.
