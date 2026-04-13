# Networking

This document covers all networking features of the Servarr Operator: Service
configuration, host port binding, Gateway API integration, TLS termination with
cert-manager, and NetworkPolicy generation.

All examples use `apiVersion: servarr.dev/v1alpha1` and `kind: ServarrApp`.

---

## Service Types

Every ServarrApp gets a Kubernetes Service. The operator defaults to `ClusterIP`
on the app's default port (e.g. 8989 for Sonarr, 7878 for Radarr, 9091 for
Transmission). You can override the service type and port list with
`spec.service`.

### ClusterIP (default)

When no `spec.service` is provided, the operator creates a ClusterIP Service
using the app's built-in default port. To explicitly set it:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  service:
    serviceType: ClusterIP
    ports:
      - name: http
        port: 8989
```

### NodePort

Expose the app on a static port across all cluster nodes:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  service:
    serviceType: NodePort
    ports:
      - name: http
        port: 8989
```

### LoadBalancer

Provision an external load balancer (cloud or MetalLB):

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  service:
    serviceType: LoadBalancer
    ports:
      - name: http
        port: 8989
```

### Multiple Ports

A service can expose more than one port. Use `containerPort` when the
container's listening port differs from the Service port:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: jellyfin
spec:
  app: Jellyfin
  service:
    serviceType: ClusterIP
    ports:
      - name: http
        port: 8096
      - name: https
        port: 8920
        containerPort: 8920
```

---

## Host Ports

When `hostPort` is set on any service port, the operator automatically switches
the Deployment strategy from `RollingUpdate` to `Recreate`. This is required
because two pods cannot bind the same host port simultaneously, so the old pod
must terminate before the new one starts.

The primary use case is Transmission's BitTorrent peer port. Peers on the
internet need a direct path to the container, which `hostPort` provides without
requiring a LoadBalancer or NodePort Service.

### Transmission Peer Port with hostPort

Transmission has dedicated `appConfig` support for peer ports. When
`peerPort.hostPort` is `true`, the operator adds TCP and UDP container ports
with `hostPort` set and switches to Recreate strategy:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: transmission
spec:
  app: Transmission
  appConfig:
    Transmission:
      peerPort:
        port: 51413
        hostPort: true
```

This produces container ports `peer-tcp` (TCP/51413) and `peer-udp`
(UDP/51413), both bound to the host.

### Generic hostPort on a Service Port

Any service port can use `hostPort` directly:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  service:
    serviceType: ClusterIP
    ports:
      - name: http
        port: 8989
        hostPort: 8989
```

When the operator detects any port with a `hostPort` value, it sets
`spec.strategy.type: Recreate` on the Deployment.

---

## Gateway API

The operator creates Gateway API route resources (HTTPRoute or TCPRoute) when
`spec.gateway.enabled` is `true`. This replaces the need for manually authored
Ingress or Route objects.

### Prerequisites

- A Gateway API implementation installed in the cluster (e.g. Envoy Gateway,
  Cilium, Istio, Traefik).
- A `Gateway` resource already provisioned in the target namespace.

### HTTPRoute (default)

By default, `routeType` is `Http` and the operator creates an HTTPRoute
(`gateway.networking.k8s.io/v1`) pointing at the app's Service on its first
port:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  gateway:
    enabled: true
    parentRefs:
      - name: my-gateway
        namespace: gateway-ns
    hosts:
      - sonarr.example.com
```

The generated HTTPRoute has:
- `parentRefs` matching the list above (including optional `sectionName` for
  listener selection).
- `hostnames` matching `spec.gateway.hosts`.
- A single rule with a `backendRef` to the app's Service.

### TCPRoute

Set `routeType: Tcp` to create a TCPRoute
(`gateway.networking.k8s.io/v1alpha2`) instead. This is useful for non-HTTP
protocols or when TLS passthrough is handled at the gateway level:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: transmission
spec:
  app: Transmission
  gateway:
    enabled: true
    routeType: Tcp
    parentRefs:
      - name: my-gateway
        namespace: gateway-ns
```

### Section Name

If the Gateway has multiple listeners, use `sectionName` to bind to a specific
one:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  gateway:
    enabled: true
    parentRefs:
      - name: my-gateway
        namespace: gateway-ns
        sectionName: https
    hosts:
      - sonarr.example.com
```

---

## TLS with cert-manager

When TLS is enabled, the operator creates a cert-manager `Certificate`
resource (`cert-manager.io/v1`) and automatically switches from HTTPRoute to
TCPRoute for TLS passthrough.

### Prerequisites

- cert-manager installed in the cluster.
- A `ClusterIssuer` (or `Issuer`) configured (e.g. `letsencrypt-prod`).

### Basic TLS

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  gateway:
    enabled: true
    parentRefs:
      - name: my-gateway
        namespace: gateway-ns
    hosts:
      - sonarr.example.com
    tls:
      enabled: true
      certIssuer: letsencrypt-prod
```

This creates:

1. A `Certificate` resource with `dnsNames: ["sonarr.example.com"]` referencing
   the `letsencrypt-prod` ClusterIssuer.
2. A TLS Secret named `sonarr-tls` (auto-derived from the app name).
3. A `TCPRoute` instead of an `HTTPRoute` (TLS forces TCP mode regardless of
   the `routeType` setting).

### Custom Secret Name

Override the auto-derived secret name:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  gateway:
    enabled: true
    parentRefs:
      - name: my-gateway
        namespace: gateway-ns
    hosts:
      - sonarr.example.com
    tls:
      enabled: true
      certIssuer: letsencrypt-prod
      secretName: my-custom-tls-secret
```

### Full TLS Example with Multiple Hosts

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: radarr
spec:
  app: Radarr
  gateway:
    enabled: true
    parentRefs:
      - name: my-gateway
        namespace: gateway-ns
        sectionName: https
    hosts:
      - radarr.example.com
      - movies.example.com
    tls:
      enabled: true
      certIssuer: letsencrypt-prod
```

Both hostnames appear as `dnsNames` on the generated Certificate. The TLS
secret is named `radarr-tls`.

---

## NetworkPolicy

The operator can generate a Kubernetes NetworkPolicy to restrict traffic to and
from the app's pods.

### Simple Mode

Set `spec.networkPolicy: true` to create a basic ingress-only policy that
allows traffic on the app's service ports from pods in the same namespace:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  networkPolicy: true
```

This generates a NetworkPolicy with:
- **Ingress**: Allow from same namespace on the app's ports.
- **Egress**: Allow to same namespace (pod-to-pod), allow DNS (UDP 53 to
  kube-dns).
- **Policy types**: Both Ingress and Egress.

### Advanced Mode

Use `spec.networkPolicyConfig` for fine-grained control. When set, it takes
precedence over the boolean `networkPolicy` flag:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: transmission
spec:
  app: Transmission
  networkPolicyConfig:
    allowSameNamespace: true
    allowDns: true
    allowInternetEgress: true
    deniedCidrBlocks:
      - "10.0.0.0/8"
      - "172.16.0.0/12"
      - "192.168.0.0/16"
```

#### Configuration Fields

| Field | Default | Description |
|---|---|---|
| `allowSameNamespace` | `true` | Allow pods in the same namespace to reach this app on its service ports. |
| `allowDns` | `true` | Allow egress to kube-dns (UDP 53) so the pod can resolve DNS. |
| `allowInternetEgress` | `false` | Allow egress to the public internet. Private CIDR blocks are denied. |
| `deniedCidrBlocks` | RFC 1918 ranges | CIDR blocks to exclude from internet egress. When empty, defaults to `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`. |
| `customEgressRules` | `[]` | Arbitrary additional egress rules as raw `NetworkPolicyEgressRule` JSON objects. |

When `allowInternetEgress` is `true`, the operator creates an egress rule
allowing `0.0.0.0/0` with an `except` list containing either the provided
`deniedCidrBlocks` or the default RFC 1918 ranges. This lets the pod reach
external services (package registries, APIs, torrent peers) while blocking
access to internal cluster and LAN networks.

### Gateway Namespace Auto-Allow

When `spec.gateway` is enabled and any `parentRef` specifies a `namespace`, the
operator automatically adds an ingress rule allowing traffic from that gateway
namespace. This means the gateway's data plane pods can reach the app without
manual NetworkPolicy exceptions:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  gateway:
    enabled: true
    parentRefs:
      - name: my-gateway
        namespace: gateway-system
    hosts:
      - sonarr.example.com
  networkPolicyConfig:
    allowSameNamespace: true
    allowDns: true
```

The generated NetworkPolicy includes an ingress rule:
```yaml
- from:
    - namespaceSelector:
        matchLabels:
          kubernetes.io/metadata.name: gateway-system
  ports:
    - port: 8989
      protocol: TCP
```

### Transmission Peer Port Ingress

When a Transmission app has `appConfig.Transmission.peerPort` configured, the
NetworkPolicy automatically allows inbound traffic from `0.0.0.0/0` on the peer
port (both TCP and UDP). This permits BitTorrent peers from the internet to
connect:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: transmission
spec:
  app: Transmission
  appConfig:
    Transmission:
      peerPort:
        port: 51413
        hostPort: true
  networkPolicyConfig:
    allowSameNamespace: true
    allowDns: true
    allowInternetEgress: true
```

The generated NetworkPolicy includes a peer-port ingress rule:
```yaml
- from:
    - ipBlock:
        cidr: 0.0.0.0/0
  ports:
    - protocol: TCP
      port: 51413
    - protocol: UDP
      port: 51413
```

### Custom Egress Rules

For cases not covered by the built-in flags, add raw NetworkPolicyEgressRule
objects:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  networkPolicyConfig:
    allowSameNamespace: true
    allowDns: true
    customEgressRules:
      - to:
          - namespaceSelector:
              matchLabels:
                kubernetes.io/metadata.name: database-ns
        ports:
          - protocol: TCP
            port: 5432
```

---

## Local Development with Envoy Gateway and nip.io

When running a local cluster (e.g. Docker Desktop or kind), you can reach every
app in a browser without `kubectl port-forward` by combining Envoy Gateway with
[nip.io](https://nip.io) hostnames.

### How it works

- **Envoy Gateway** creates a `LoadBalancer` Service for your `Gateway` resource.
  Docker Desktop maps LoadBalancer services to `localhost` / `127.0.0.1`.
- **nip.io** is a public wildcard DNS service. A hostname like
  `sonarr.127.0.0.1.nip.io` resolves to `127.0.0.1` via public DNS — no local
  `/etc/hosts` edits, no dnsmasq, no VPN.

### Step 1 — Install Envoy Gateway

```bash
helm install envoy-gateway oci://docker.io/envoyproxy/gateway-helm \
  --version v1.7.0 \
  --namespace envoy-gateway-system --create-namespace

kubectl wait --timeout=5m \
  -n envoy-gateway-system deployment/envoy-gateway --for=condition=Available
```

> If the Gateway API CRDs are already installed in your cluster (e.g. by
> another tool), add `--skip-crds` to avoid field-manager conflicts.

### Step 2 — Create a GatewayClass and Gateway

Apply the following manifest:

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: GatewayClass
metadata:
  name: envoy-gateway
spec:
  controllerName: gateway.envoyproxy.io/gatewayclass-controller
---
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: servarr-gateway
  namespace: servarr-system
spec:
  gatewayClassName: envoy-gateway
  listeners:
    - name: http
      protocol: HTTP
      port: 80
```

```bash
kubectl apply -f gateway.yaml
```

Envoy Gateway creates a LoadBalancer Service for the Gateway and Docker Desktop
assigns it `localhost` / `127.0.0.1`.

### Step 3 — Add gateway config to each app

Point every app at the Gateway with a nip.io hostname:

```yaml
gateway:
  enabled: true
  parentRefs:
    - name: servarr-gateway
      namespace: servarr-system
      sectionName: http
  hosts:
    - sonarr.127.0.0.1.nip.io
```

For `split4k` apps (Sonarr, Radarr) use `split4kOverrides.gateway` to assign a
separate host to the 4K instance:

```yaml
- app: Sonarr
  split4k: true
  gateway:
    enabled: true
    parentRefs:
      - name: servarr-gateway
        namespace: servarr-system
        sectionName: http
    hosts:
      - sonarr.127.0.0.1.nip.io
  split4kOverrides:
    gateway:
      enabled: true
      parentRefs:
        - name: servarr-gateway
          namespace: servarr-system
          sectionName: http
      hosts:
        - sonarr-4k.127.0.0.1.nip.io
```

The `split4kOverrides.gateway` field lets each instance have its own hostname with no extra resources.

### Step 4 — Verify

```bash
# Gateway should show address 127.0.0.1 / localhost
kubectl get gateway -n servarr-system servarr-gateway

# One HTTPRoute per app
kubectl get httproute -n servarr-system

# Open in browser (no port needed)
open http://sonarr.127.0.0.1.nip.io
open http://sonarr-4k.127.0.0.1.nip.io
```

### App hostname reference

| App | URL |
|-----|-----|
| Plex | http://plex.127.0.0.1.nip.io |
| Jellyfin | http://jellyfin.127.0.0.1.nip.io |
| Sabnzbd | http://sabnzbd.127.0.0.1.nip.io |
| Transmission | http://transmission.127.0.0.1.nip.io |
| Sonarr | http://sonarr.127.0.0.1.nip.io |
| Sonarr 4K | http://sonarr-4k.127.0.0.1.nip.io |
| Radarr | http://radarr.127.0.0.1.nip.io |
| Radarr 4K | http://radarr-4k.127.0.0.1.nip.io |
| Lidarr | http://lidarr.127.0.0.1.nip.io |
| Prowlarr | http://prowlarr.127.0.0.1.nip.io |
| Overseerr | http://overseerr.127.0.0.1.nip.io |
| Tautulli | http://tautulli.127.0.0.1.nip.io |
| Maintainerr | http://maintainerr.127.0.0.1.nip.io |

---

## Combining Features

A complete example with service, gateway, TLS, and network policy:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  service:
    serviceType: ClusterIP
    ports:
      - name: http
        port: 8989
  gateway:
    enabled: true
    parentRefs:
      - name: prod-gateway
        namespace: gateway-system
        sectionName: https
    hosts:
      - sonarr.example.com
    tls:
      enabled: true
      certIssuer: letsencrypt-prod
  networkPolicyConfig:
    allowSameNamespace: true
    allowDns: true
    allowInternetEgress: false
```

This produces:
- A `ClusterIP` Service on port 8989.
- A cert-manager `Certificate` for `sonarr.example.com`.
- A `TCPRoute` attached to the `prod-gateway` on the `https` listener.
- A `NetworkPolicy` allowing ingress from the same namespace and from
  `gateway-system`, with DNS egress and same-namespace pod-to-pod egress.
