# Hardware Transcoding with NFD-based GPU Scheduling

This guide explains how to enable hardware GPU transcoding for Jellyfin or Plex using
Node Feature Discovery (NFD) to automatically schedule pods on GPU-capable nodes.

## Why NFD?

Kubernetes schedules pods based on node labels. Without automation, you would need to
manually add labels like `gpu.intel.com/i915=true` to nodes and keep them in sync as
hardware changes. NFD eliminates this: it runs a DaemonSet that inspects each node's
hardware and kernel state, then applies labels automatically.

The operator builds on NFD in two layers:

1. **NFD worker** detects raw hardware: PCI device class/vendor and loaded kernel modules.
2. **NodeFeatureRule CRs** (included in this chart) translate raw NFD labels into semantic
   GPU labels (`gpu.intel.com/i915`, `gpu.nvidia.com/present`, `gpu.amd.com/present`).

When a `ServarrApp` has `spec.gpu` set, the operator adds:
- the GPU extended resource request/limit (so Kubernetes only places the pod on a node
  with a device plugin advertising capacity)
- the NFD semantic label as a `nodeSelector` entry (so the pod only lands on a node where
  the driver is actually loaded)

## Step 1 — Install NFD

**Option A: via the operator Helm chart (recommended)**

```yaml
# values.yaml
nfd:
  enabled: true
  gpuRules:
    enabled: true   # installs NodeFeatureRule CRs for semantic GPU labels
```

```sh
helm upgrade --install servarr-operator oci://ghcr.io/phaedrus1992/charts/servarr-operator \
  --namespace servarr-system --create-namespace \
  -f values.yaml
```

**Option B: standalone NFD install**

```sh
helm install nfd oci://registry.k8s.io/nfd/charts/node-feature-discovery \
  --namespace node-feature-discovery --create-namespace \
  --version 0.18.3
```

Then apply the NodeFeatureRule CRs separately (rendered from this chart's templates with
`helm template --set nfd.enabled=true --set nfd.gpuRules.enabled=true`).

## Step 2 — Install a GPU device plugin

NFD labels nodes but doesn't advertise GPU capacity. A device plugin is required so
Kubernetes knows how many GPU units a node can supply.

| Vendor | Plugin | Extended resource key |
|--------|--------|-----------------------|
| Intel  | [Intel Device Plugins for Kubernetes](https://github.com/intel/intel-device-plugins-for-kubernetes) | `gpu.intel.com/i915` |
| NVIDIA | [NVIDIA GPU Operator](https://docs.nvidia.com/datacenter/cloud-native/gpu-operator/overview.html) | `nvidia.com/gpu` |
| AMD    | [AMD GPU Device Plugin](https://github.com/RadeonOpenCompute/k8s-device-plugin) | `amd.com/gpu` |

Install the plugin for your GPU vendor before configuring apps.

## Step 3 — How labels are produced

With `nfd.gpuRules.enabled=true`, three `NodeFeatureRule` CRs are installed. Each rule
requires **both** a matching PCI device and a loaded kernel module:

| Rule | PCI vendor | Kernel module | Label applied |
|------|-----------|---------------|---------------|
| `gpu-intel-i915` | `8086` (Intel), class `03xx` | `i915` | `gpu.intel.com/i915=true` |
| `gpu-nvidia`     | `10de` (NVIDIA), class `03xx` | `nvidia` | `gpu.nvidia.com/present=true` |
| `gpu-amd`        | `1002` (AMD), class `03xx` | `amdgpu` | `gpu.amd.com/present=true` |

Requiring both conditions prevents false positives (e.g., a PCI device present but driver
not loaded) and ensures the transcoding pipeline is actually available.

## Step 4 — Configure hardware transcoding in a MediaStack app

Set `spec.apps[].gpu` in your `MediaStack`:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: MediaStack
metadata:
  name: media
  namespace: servarr
spec:
  apps:
    - app: Jellyfin
      gpu:
        intel: 1    # Intel Quick Sync — sets gpu.intel.com/i915: 1 resource
      resources:
        requests:
          cpu: 500m
          memory: 1Gi
        limits:
          cpu: "4"
          memory: 4Gi
```

Use only one vendor field per app. The operator translates each non-zero field into:

- A resource limit+request on the container (`gpu.intel.com/i915: 1`)
- A `nodeSelector` entry on the pod (`gpu.intel.com/i915: "true"`)

The `nodeSelector` uses AND semantics: the pod lands only on a node that satisfies all
entries. Setting multiple GPU vendor fields is technically supported but unusual — it would
require a node with all three GPU types simultaneously.

For `ServarrApp` (single-app CRs), the same `spec.gpu` field applies:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: jellyfin
spec:
  app: Jellyfin
  gpu:
    intel: 1
```

## Step 5 — Verify the setup

**Check NFD labels on nodes:**

```sh
kubectl get nodes --show-labels | tr ',' '\n' | grep gpu
```

Expected output on an Intel GPU node:
```
gpu.intel.com/i915=true
```

**Check that the device plugin is advertising capacity:**

```sh
kubectl get nodes -o json | \
  jq '.items[] | {name: .metadata.name, capacity: (.status.capacity | with_entries(select(.key | startswith("gpu."))))}'
```

Expected output:
```json
{
  "name": "my-node",
  "capacity": {
    "gpu.intel.com/i915": "1"
  }
}
```

**Check that the pod scheduled and has the resource:**

```sh
kubectl describe pod -n servarr $(kubectl get pod -n servarr -l app.kubernetes.io/name=jellyfin -o name | head -1) \
  | grep -A10 "Limits:"
```

Expected output:
```
Limits:
  cpu:               4
  gpu.intel.com/i915: 1
  memory:            4Gi
```

**Check the pod's nodeSelector:**

```sh
kubectl get pod -n servarr -l app.kubernetes.io/name=jellyfin -o jsonpath='{.items[0].spec.nodeSelector}' | jq
```

Expected output:
```json
{
  "gpu.intel.com/i915": "true"
}
```

## Troubleshooting

**Pod stays Pending with `Insufficient gpu.intel.com/i915`**

The device plugin is not installed, or isn't detecting the GPU. Check:

```sh
kubectl get pods -n intel-device-plugins  # or whichever namespace your plugin uses
kubectl describe node <your-node> | grep -A5 "Allocatable"
```

If `gpu.intel.com/i915` doesn't appear under Allocatable, the device plugin isn't running
or the i915 driver isn't loaded on that node.

**NFD labels are missing (`gpu.intel.com/i915` not present on node)**

Check that NFD worker pods are running:

```sh
kubectl get pods -n node-feature-discovery
```

Check that the NodeFeatureRule CRs were installed:

```sh
kubectl get nodefeaturerules
```

Check the i915 module is loaded on the node:

```sh
# On the node (or via a privileged debug pod)
lsmod | grep i915
```

If the module is not loaded, either the node doesn't have an Intel GPU, or the driver
needs to be installed.

**NodeFeatureRule CRs not found (no `nfd.k8s.io/v1alpha1` API)**

The NodeFeatureRule CRD requires NFD ≥ 0.14. Check NFD version:

```sh
kubectl get crd nodefeaturerules.nfd.k8s.io -o jsonpath='{.metadata.annotations}'
```

If missing, upgrade or reinstall NFD.
