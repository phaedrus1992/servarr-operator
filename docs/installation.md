# Installation

This guide covers installing the Servarr Operator Helm chart into a Kubernetes cluster.

## Prerequisites

1. **Kubernetes 1.27+** -- verify your cluster version:

   ```bash
   kubectl version --short
   ```

2. **Helm 3** -- verify Helm is installed:

   ```bash
   helm version
   ```

3. **cert-manager** (optional, required if webhooks are enabled) -- the operator uses
   cert-manager to provision TLS certificates for its validating webhook. Install it
   before the operator:

   ```bash
   kubectl apply -f https://github.com/cert-manager/cert-manager/releases/latest/download/cert-manager.yaml
   ```

4. **Gateway API CRDs** (optional, for ingress routing) -- if your `ServarrApp`
   resources will use Gateway API for HTTP routing, install the CRDs first:

   ```bash
   kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/latest/download/standard-install.yaml
   ```

5. **Velero** (optional, for volume-level backups) -- if you want to back up
   ServarrApp config PVCs at the volume level (in addition to the operator's
   built-in Servarr API backups), install Velero:

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

   See [Backup and Restore](backup-restore.md#volume-level-backups-with-velero)
   for full configuration details including storage locations and schedules.

## Install CRDs

The operator's Custom Resource Definitions are packaged as a separate Helm chart.
This step requires cluster-admin privileges and only needs to be done once per
cluster.

```bash
helm install servarr-crds \
  oci://ghcr.io/phaedrus1992/servarr/servarr-crds
```

To configure the validating webhook, set the namespace where the operator will
be installed:

```bash
helm install servarr-crds \
  oci://ghcr.io/phaedrus1992/servarr/servarr-crds \
  --set operatorNamespace=servarr
```

To disable webhooks (removes the cert-manager dependency for CRDs):

```bash
helm install servarr-crds \
  oci://ghcr.io/phaedrus1992/servarr/servarr-crds \
  --set webhook.enabled=false
```

## Install the Operator

1. Create a namespace for the operator:

   ```bash
   kubectl create namespace servarr
   ```

2. Install the Helm chart:

   ```bash
   helm install servarr-operator \
     oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
     --namespace servarr
   ```

   To pin a specific chart version:

   ```bash
   helm install servarr-operator \
     oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
     --namespace servarr \
     --version 0.1.0
   ```

### Cluster-Scoped Mode

By default the operator watches only its own namespace and uses `Role`/`RoleBinding`
privileges. To watch all namespaces (requires `ClusterRole`/`ClusterRoleBinding`):

```bash
helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr \
  --set watchAllNamespaces=true
```

The CRDs chart still requires a one-time cluster-admin install regardless of
which mode the operator runs in.

## Release Channels

The operator is published to `ghcr.io` in two channels: stable releases and
snapshot builds.

### Stable Releases

Stable versions are published when a Git tag (`v*`) is pushed. Both the
container image and Helm charts use semver (e.g. `1.0.0`):

```bash
# Install a specific stable release
helm install servarr-crds \
  oci://ghcr.io/phaedrus1992/servarr/servarr-crds \
  --version 1.0.0

helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr \
  --version 1.0.0
```

Omitting `--version` installs the latest stable release.

### Snapshot Builds

Every push to `main` publishes a snapshot build. Container images are tagged
`snapshot` (floating) and `snapshot-YYYYMMDD` (date-stamped). Helm charts use
version `0.0.0-snapshot.YYYYMMDD`.

```bash
# Install the latest snapshot
helm install servarr-crds \
  oci://ghcr.io/phaedrus1992/servarr/servarr-crds \
  --version 0.0.0-snapshot.20260219

helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr \
  --version 0.0.0-snapshot.20260219
```

To use the container image directly without Helm:

```bash
docker pull ghcr.io/phaedrus1992/servarr-operator:snapshot
# or a specific date:
docker pull ghcr.io/phaedrus1992/servarr-operator:snapshot-20260219
```

Snapshot builds older than 7 days are automatically cleaned up.

## Helm Values Reference

Below are the key values you can override. See `charts/servarr-operator/values.yaml`
for the full file.

### image

| Key | Default | Description |
|-----|---------|-------------|
| `image.repository` | `ghcr.io/phaedrus1992/servarr-operator` | Operator container image |
| `image.tag` | `""` (defaults to `appVersion`) | Image tag |
| `image.pullPolicy` | `IfNotPresent` | Image pull policy |

```bash
helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr \
  --set image.tag=0.1.0
```

### resources

| Key | Default | Description |
|-----|---------|-------------|
| `resources.limits.cpu` | `500m` | CPU limit |
| `resources.limits.memory` | `256Mi` | Memory limit |
| `resources.requests.cpu` | `50m` | CPU request |
| `resources.requests.memory` | `64Mi` | Memory request |

### watchAllNamespaces

| Key | Default | Description |
|-----|---------|-------------|
| `watchAllNamespaces` | `false` | Watch all namespaces (uses ClusterRole/ClusterRoleBinding). Default is namespace-scoped (Role/RoleBinding). |

### webhook

| Key | Default | Description |
|-----|---------|-------------|
| `webhook.enabled` | `true` | Enable the validating admission webhook |
| `webhook.certIssuer` | `selfsigned-issuer` | cert-manager issuer name |
| `webhook.certIssuerKind` | `ClusterIssuer` | cert-manager issuer kind |

To disable webhooks (removes the cert-manager dependency):

```bash
helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr \
  --set webhook.enabled=false
```

### nodeSelector and tolerations

| Key | Default | Description |
|-----|---------|-------------|
| `nodeSelector` | `{}` | Node selector labels for pod scheduling |
| `tolerations` | `[]` | Tolerations for pod scheduling |

```bash
helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr \
  --set nodeSelector.kubernetes\\.io/arch=amd64
```

### defaultImages

Default container images for each managed application. These are used when a
`ServarrApp` resource does not specify an explicit image. The full list of defaults:

| App | Repository | Tag |
|-----|-----------|-----|
| plex | `linuxserver/plex` | `1.41.4` |
| jellyfin | `linuxserver/jellyfin` | `10.10.7` |
| ssh-bastion | `quay.io/panubo/sshd` | `1.10.0` |
| sabnzbd | `linuxserver/sabnzbd` | `4.5.5` |
| transmission | `linuxserver/transmission` | `4.1.0` |
| sonarr | `linuxserver/sonarr` | `4.0.16` |
| radarr | `linuxserver/radarr` | `6.0.4` |
| lidarr | `linuxserver/lidarr` | `2.9.6` |
| tautulli | `linuxserver/tautulli` | `2.16.0` |
| overseerr | `linuxserver/overseerr` | `1.34.0` |
| maintainerr | `ghcr.io/jorenn92/maintainerr` | `2.19.0` |
| prowlarr | `linuxserver/prowlarr` | `2.3.0` |
| jackett | `linuxserver/jackett` | `0.24.988` |

To override a default image:

```bash
helm install servarr-operator \
  oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
  --namespace servarr \
  --set defaultImages.sonarr.tag=4.0.17
```

## Verify the Installation

1. Check that the operator pod is running:

   ```bash
   kubectl get pods -n servarr
   ```

   Expected output:

   ```
   NAME                                 READY   STATUS    RESTARTS   AGE
   servarr-operator-xxxxxxxxxx-xxxxx    1/1     Running   0          30s
   ```

2. Check the operator logs:

   ```bash
   kubectl logs -n servarr deployment/servarr-operator
   ```

3. If webhooks are enabled, verify the certificate was issued:

   ```bash
   kubectl get certificate -n servarr
   ```

## Upgrading

1. Upgrade CRDs first (if the new version includes CRD changes):

   ```bash
   helm upgrade servarr-crds \
     oci://ghcr.io/phaedrus1992/servarr/servarr-crds
   ```

2. Upgrade the operator:

   ```bash
   helm upgrade servarr-operator \
     oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
     --namespace servarr
   ```

   To upgrade to a specific version:

   ```bash
   helm upgrade servarr-operator \
     oci://ghcr.io/phaedrus1992/servarr/servarr-operator \
     --namespace servarr \
     --version 1.1.0
   ```

3. Verify the rollout completed:

   ```bash
   kubectl rollout status deployment/servarr-operator -n servarr
   ```

## Uninstalling

1. Remove the Helm release:

   ```bash
   helm uninstall servarr-operator --namespace servarr
   ```

2. Optionally delete the namespace:

   ```bash
   kubectl delete namespace servarr
   ```

   Note: CRDs are not removed by `helm uninstall`. To remove them manually:

   ```bash
   kubectl get crds -o name | grep servarr.dev | xargs kubectl delete
   ```
