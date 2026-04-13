# Backup and Restore

## Overview

The Servarr Operator can trigger application-level backups through the native
Servarr v3 REST API. This covers the application database and configuration
files -- it is **not** a volume-level or PVC snapshot mechanism.

Supported app types (Servarr v3):

- Sonarr
- Radarr
- Lidarr
- Prowlarr

The operator calls the `/api/v3/system/backup` endpoints during its
reconciliation loop whenever the configured cron schedule fires. Backup
metadata (last run time, result, count) is reported in the ServarrApp status,
and Prometheus metrics are emitted for every backup and restore operation.

## Prerequisites

Backups require a valid API key so the operator can authenticate against the
app. You must create a Kubernetes Secret containing the key and reference it
in the ServarrApp spec.

### 1. Create the API key Secret

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: sonarr-api-key
  namespace: media
type: Opaque
stringData:
  api-key: "your-sonarr-api-key-here"
```

The Secret **must** contain a data field named `api-key`.

### 2. Reference the Secret in the ServarrApp

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
  namespace: media
spec:
  app: Sonarr
  apiKeySecret: sonarr-api-key
```

Without `apiKeySecret`, the operator cannot reach the app API and backup
operations will not run.

## Enabling Backups

Add a `backup` section to the ServarrApp spec:

| Field                       | Type   | Default | Description                                           |
|-----------------------------|--------|---------|-------------------------------------------------------|
| `spec.backup.enabled`       | bool   | false   | Enable automated backups.                             |
| `spec.backup.schedule`      | string | ""      | Cron expression controlling when backups run.         |
| `spec.backup.retentionCount`| int    | 5       | Number of backups to keep. Oldest are pruned first.   |

### Full Example

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
  namespace: media
spec:
  app: Sonarr
  apiKeySecret: sonarr-api-key

  backup:
    enabled: true
    schedule: "0 3 * * *"      # daily at 03:00
    retentionCount: 7           # keep one week of backups

  persistence:
    volumes:
      - name: config
        mountPath: /config
        size: 5Gi
```

### Multiple Apps

Each ServarrApp CR has its own backup settings. A typical media stack might
look like:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
  namespace: media
spec:
  app: Sonarr
  apiKeySecret: sonarr-api-key
  backup:
    enabled: true
    schedule: "0 3 * * *"
    retentionCount: 7
---
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: radarr
  namespace: media
spec:
  app: Radarr
  apiKeySecret: radarr-api-key
  backup:
    enabled: true
    schedule: "15 3 * * *"
    retentionCount: 7
---
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: prowlarr
  namespace: media
spec:
  app: Prowlarr
  apiKeySecret: prowlarr-api-key
  backup:
    enabled: true
    schedule: "30 3 * * *"
    retentionCount: 5
```

## How It Works

1. On each reconciliation, the operator evaluates the cron expression in
   `spec.backup.schedule` against the current time and the last recorded
   backup time.
2. When the schedule fires, the operator sends a `POST /api/v3/system/backup`
   request to the app using the API key from `apiKeySecret`.
3. The app creates an internal backup (database + configuration) and returns
   backup metadata (id, name, path, size, timestamp).
4. The operator records the result in `status.backupStatus` on the ServarrApp
   resource.
5. If the number of existing backups exceeds `retentionCount`, the operator
   deletes the oldest backups via `DELETE /api/v3/system/backup/{id}` until
   the count is within the limit.

## Monitoring

### Status Field

Backup status is available in the ServarrApp status subresource:

```bash
kubectl get sa sonarr -n media -o yaml
```

Relevant fields under `status.backupStatus`:

| Field              | Description                                       |
|--------------------|---------------------------------------------------|
| `lastBackupTime`   | ISO 8601 timestamp of the most recent backup.     |
| `lastBackupResult` | Result of the last backup operation (e.g. "Success", "Failed"). |
| `backupCount`      | Number of backups currently stored in the app.     |

Example status output:

```yaml
status:
  ready: true
  backupStatus:
    lastBackupTime: "2026-02-17T03:00:12Z"
    lastBackupResult: "Success"
    backupCount: 7
```

### Prometheus Metrics

The operator exposes the following metric for backup and restore operations:

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `servarr_operator_backup_operations_total` | Counter | `app_type`, `operation`, `result` | Total backup and restore operations. |

Label values:

- `operation`: `"backup"` or `"restore"`
- `result`: `"success"` or `"error"`
- `app_type`: `"sonarr"`, `"radarr"`, `"lidarr"`, or `"prowlarr"`

Example PromQL queries:

```promql
# Backup failures in the last 24 hours
increase(servarr_operator_backup_operations_total{operation="backup", result="error"}[24h])

# Successful backups per app type
sum by (app_type) (servarr_operator_backup_operations_total{operation="backup", result="success"})
```

## Restoring from Backup

Restore is triggered by annotating the ServarrApp resource with
`servarr.dev/restore-from` set to the numeric backup ID.

### Step 1: List available backups

The backup ID is the integer `id` field returned by the Servarr API. You can
query it directly from the app:

```bash
# Port-forward to the app
kubectl port-forward -n media svc/sonarr 8989:8989

# List backups (requires the API key)
curl -s -H "X-Api-Key: <api-key>" http://localhost:8989/api/v3/system/backup | jq '.[] | {id, name, time}'
```

Example output:

```json
{ "id": 3, "name": "nzbdrone_backup_v4.0.0.700_2026.02.17_0300.zip", "time": "2026-02-17T03:00:12Z" }
{ "id": 2, "name": "nzbdrone_backup_v4.0.0.700_2026.02.16_0300.zip", "time": "2026-02-16T03:00:08Z" }
{ "id": 1, "name": "nzbdrone_backup_v4.0.0.700_2026.02.15_0300.zip", "time": "2026-02-15T03:00:11Z" }
```

### Step 2: Annotate the ServarrApp

```bash
kubectl annotate servarrapp sonarr -n media servarr.dev/restore-from=3
```

Or apply it declaratively:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
  namespace: media
  annotations:
    servarr.dev/restore-from: "3"
spec:
  app: Sonarr
  apiKeySecret: sonarr-api-key
  backup:
    enabled: true
    schedule: "0 3 * * *"
    retentionCount: 7
```

### Step 3: Operator handles the restore

On the next reconciliation the operator:

1. Reads the `servarr.dev/restore-from` annotation.
2. Parses the value as an integer backup ID.
3. Calls `POST /api/v3/system/backup/restore/{id}` against the app.
4. Removes the annotation from the ServarrApp to prevent re-triggering.
5. Records the restore result in Prometheus metrics
   (`servarr_operator_backup_operations_total` with `operation="restore"`).

The app will restart automatically as part of its internal restore process.

## Limitations

- **Servarr v3 apps only.** Backup and restore are implemented through the
  Servarr v3 REST API (`/api/v3/system/backup`). The following app types do
  **not** expose a compatible backup API and are not supported:

  - Transmission
  - SABnzbd
  - Tautulli
  - Overseerr
  - Maintainerr
  - Jackett
  - Jellyfin

  If `spec.backup.enabled` is set to `true` on a non-Servarr-v3 app, the
  operator will log a warning and skip backup operations.

- **Application-level only.** These backups cover the app database and
  configuration. Media files, download directories, and PVC data are not
  included. Use volume snapshots or external backup tools for those.

- **Single-instance assumption.** The backup API operates on the running
  instance. If the pod is not healthy or not yet ready, the backup call will
  fail and be retried on the next reconciliation that matches the schedule.

- **Restore is disruptive.** Restoring causes the application to restart. Any
  in-progress downloads or indexer operations will be interrupted.

## Volume-Level Backups with Velero

The operator's built-in backup feature covers application databases (Servarr v3
apps only). For full disaster recovery -- including config PVCs, non-Servarr
apps, and all Kubernetes resources -- use [Velero](https://velero.io/).

### Installing Velero

Install Velero using the Helm chart with the node-agent DaemonSet for
file-system-level PVC backups:

```bash
helm repo add vmware-tanzu https://vmware-tanzu.github.io/helm-charts
helm install velero vmware-tanzu/velero \
  --namespace velero --create-namespace \
  --set deployNodeAgent=true \
  --set "initContainers[0].name=velero-plugin-for-aws" \
  --set "initContainers[0].image=velero/velero-plugin-for-aws:v1.11.1" \
  --set "initContainers[0].volumeMounts[0].mountPath=/target" \
  --set "initContainers[0].volumeMounts[0].name=plugins"
```

### Configuring Backup Storage

Create an S3 credentials secret and a `BackupStorageLocation`:

1. **Create credentials**:

   ```yaml
   apiVersion: v1
   kind: Secret
   metadata:
     name: velero-credentials
     namespace: velero
   type: Opaque
   stringData:
     cloud: |
       [default]
       aws_access_key_id=YOUR_ACCESS_KEY
       aws_secret_access_key=YOUR_SECRET_KEY
   ```

2. **Create the BSL** -- for S3-compatible backends (MinIO, Synology C2, etc.)
   set `s3Url` and `s3ForcePathStyle`:

   ```yaml
   apiVersion: velero.io/v1
   kind: BackupStorageLocation
   metadata:
     name: default
     namespace: velero
   spec:
     provider: aws
     objectStorage:
       bucket: your-bucket-name
     credential:
       name: velero-credentials
       key: cloud
     config:
       region: us-east-1
       # s3Url: https://your-s3-endpoint
       # s3ForcePathStyle: "true"
   ```

3. **Verify** the BSL is available:

   ```bash
   velero backup-location get
   ```

### Backup Schedules

Example Velero schedules for a servarr namespace:

| Schedule | Cron | Retention | Scope |
|----------|------|-----------|-------|
| `servarr-config-nightly` | `0 4 * * *` (daily 04:00) | 7 days | PVCs and PVs in the `servarr` namespace |
| `servarr-full-weekly` | `0 5 * * 0` (Sundays 05:00) | 30 days | All resources + volumes in the `servarr` namespace |

Create them with the Velero CLI:

```bash
velero schedule create servarr-config-nightly \
  --schedule="0 4 * * *" \
  --ttl 168h \
  --include-namespaces servarr \
  --include-resources persistentvolumeclaims,persistentvolumes \
  --default-volumes-to-fs-backup

velero schedule create servarr-full-weekly \
  --schedule="0 5 * * 0" \
  --ttl 720h \
  --include-namespaces servarr \
  --default-volumes-to-fs-backup
```

The `--default-volumes-to-fs-backup` flag uses the Velero node-agent to copy
PVC contents at the file-system level. This works with any storage provider
(NFS, OpenEBS hostpath, etc.) without requiring CSI snapshot support.

### Manual Backup

Trigger an on-demand backup at any time:

```bash
velero backup create servarr-manual \
  --include-namespaces servarr \
  --default-volumes-to-fs-backup \
  --wait
```

Check status:

```bash
velero backup describe servarr-manual --details
```

### Restoring from a Velero Backup

1. List available backups:

   ```bash
   velero backup get
   ```

2. Restore into the same namespace:

   ```bash
   velero restore create --from-backup servarr-config-nightly-YYYYMMDDHHMMSS --wait
   ```

3. Verify pods are running:

   ```bash
   kubectl get pods -n servarr
   ```

Velero will recreate PVCs and restore their contents from the backup. Existing
resources that match are skipped by default; use `--existing-resource-policy=update`
to overwrite them.

### Combining Both Backup Strategies

For best coverage, use both approaches together:

- **Operator backups** (Servarr v3 API) run more frequently, are fast, and
  produce portable ZIP archives inside the app. They protect against
  application-level corruption and allow restoring to a specific point without
  touching the volume.

- **Velero backups** (volume-level) capture the full PVC state for all apps
  including non-Servarr ones (Transmission, SABnzbd, Jellyfin, etc.). They
  protect against volume loss, node failure, and cluster-level disasters.

A recommended schedule:

| Layer | Tool | Frequency | Retention |
|-------|------|-----------|-----------|
| Application DB | Operator `spec.backup` | Every 6-12 hours | 7-14 copies |
| Config PVCs | Velero nightly schedule | Daily | 7 days |
| Full namespace | Velero weekly schedule | Weekly | 30 days |
