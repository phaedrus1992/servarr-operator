# GPU-Aware Scheduling with Node Feature Discovery

The operator supports hardware GPU transcoding for Jellyfin and Plex via the `gpu` field in
the `ServarrApp` / `MediaStack` spec. This field maps to Kubernetes extended resource requests,
which require a device plugin to advertise capacity. Node Feature Discovery (NFD) provides the
prerequisite node labels that GPU device plugins depend on for selection.

## NFD Helm subchart

The `servarr-operator` chart includes NFD as an optional dependency. To install NFD alongside
the operator:

```yaml
# values.yaml
nfd:
  enabled: true
```

Or as a standalone installation:

```sh
helm install nfd oci://registry.k8s.io/nfd/charts/node-feature-discovery \
  --namespace node-feature-discovery --create-namespace \
  --version 0.18.3
```

## Node labels produced by NFD

With the default worker config included in this chart, NFD produces the following labels:

### PCI device labels (`pci-<class>_<vendor>.present`)

| GPU family      | Label                                                     |
|-----------------|-----------------------------------------------------------|
| Intel (display) | `feature.node.kubernetes.io/pci-0300_8086.present`        |
| Intel (3D ctrl) | `feature.node.kubernetes.io/pci-0302_8086.present`        |
| NVIDIA          | `feature.node.kubernetes.io/pci-0300_10de.present`        |
| AMD             | `feature.node.kubernetes.io/pci-0300_1002.present`        |

PCI class codes:
- `0300` — VGA compatible controller / Display controller
- `0302` — 3D controller (used by some integrated GPUs and discrete chips)

### Kernel module labels (`kernel-loadedmodule.<module>`)

| Driver             | Label                                                          |
|--------------------|----------------------------------------------------------------|
| Intel i915         | `feature.node.kubernetes.io/kernel-loadedmodule.i915`         |
| Intel Xe (newer)   | `feature.node.kubernetes.io/kernel-loadedmodule.xe`           |
| NVIDIA proprietary | `feature.node.kubernetes.io/kernel-loadedmodule.nvidia`       |
| AMD open-source    | `feature.node.kubernetes.io/kernel-loadedmodule.amdgpu`       |

## Device plugins

NFD labels alone don't advertise GPU capacity — you also need a device plugin per vendor:

| Vendor | Plugin                                             |
|--------|----------------------------------------------------|
| Intel  | [Intel Device Plugins](https://github.com/intel/intel-device-plugins-for-kubernetes) — installs `gpu.intel.com/i915` or `gpu.intel.com/xe` |
| NVIDIA | [NVIDIA GPU Operator](https://docs.nvidia.com/datacenter/cloud-native/gpu-operator/overview.html) — installs `nvidia.com/gpu` |
| AMD    | [AMD GPU Device Plugin](https://github.com/RadeonOpenCompute/k8s-device-plugin) — installs `amd.com/gpu` |

## ServarrApp GPU spec

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: jellyfin
spec:
  app: Jellyfin
  gpu:
    intel: 1    # requests gpu.intel.com/i915: 1
    # nvidia: 1 # requests nvidia.com/gpu: 1
    # amd: 1    # requests amd.com/gpu: 1
```

Only one vendor field should be set per app. The operator translates each field to the
corresponding extended resource request in the pod spec.

## Verifying NFD is working

After NFD is installed and a node has an Intel GPU:

```sh
# Check NFD labels on nodes
kubectl get nodes -o json | \
  jq '.items[].metadata.labels | with_entries(select(.key | startswith("feature.node.kubernetes.io")))'

# Verify Intel device plugin is advertising capacity
kubectl get nodes -o json | \
  jq '.items[].status.capacity | with_entries(select(.key | startswith("gpu.intel.com")))'
```
