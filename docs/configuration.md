# ServarrApp CRD Field Reference

**API Group:** `servarr.dev`
**Version:** `v1alpha1`
**Kind:** `ServarrApp`
**Short Name:** `sa`
**Scope:** Namespaced

---

## Top-Level Spec Fields

| Field | Type | Required | Default |
|---|---|---|---|
| `app` | `AppType` | Yes | -- |
| `instance` | `string` | No | -- |
| `image` | `ImageSpec` | No | Per-app defaults |
| `uid` | `int64` | No | `65534` |
| `gid` | `int64` | No | `65534` |
| `security` | `SecurityProfile` | No | Per-app defaults |
| `service` | `ServiceSpec` | No | Per-app defaults |
| `gateway` | `GatewaySpec` | No | -- |
| `resources` | `ResourceRequirements` | No | limits: 1 cpu / 512Mi, requests: 100m / 128Mi |
| `persistence` | `PersistenceSpec` | No | Per-app defaults |
| `env` | `[]EnvVar` | No | `[{name: TZ, value: UTC}]` |
| `probes` | `ProbeSpec` | No | HTTP `/` with defaults |
| `scheduling` | `NodeScheduling` | No | -- |
| `networkPolicy` | `bool` | No | -- |
| `networkPolicyConfig` | `NetworkPolicyConfig` | No | -- |
| `appConfig` | `AppConfig` | No | -- |
| `apiKeySecret` | `string` | No | -- |
| `apiHealthCheck` | `ApiHealthCheckSpec` | No | -- |
| `backup` | `BackupSpec` | No | -- |
| `imagePullSecrets` | `[]string` | No | -- |
| `podAnnotations` | `map[string]string` | No | -- |
| `gpu` | `GpuSpec` | No | -- |
| `prowlarrSync` | `ProwlarrSyncSpec` | No | -- |
| `overseerrSync` | `OverseerrSyncSpec` | No | -- |

---

## Field Details

### `app`

**Type:** `AppType` (enum) -- **Required**

Selects which application this resource manages. The operator uses this to determine default images, ports, security profiles, and volume layouts.

Valid values: `Plex`, `Jellyfin`, `SshBastion`, `Sabnzbd`, `Transmission`, `Sonarr`, `Radarr`, `Lidarr`, `Tautulli`, `Overseerr`, `Maintainerr`, `Prowlarr`, `Jackett`

```yaml
spec:
  app: Sonarr
```

---

### `instance`

**Type:** `string` -- **Optional**

Label to distinguish multiple instances of the same app type within a namespace. Appended to generated resource names (e.g. `sonarr-4k`).

```yaml
spec:
  app: Sonarr
  instance: "4k"
```

---

### `image`

**Type:** `ImageSpec` -- **Optional**

Override the default container image. When omitted, the operator uses a built-in default per `app` type.

| Sub-field | Type | Default |
|---|---|---|
| `repository` | `string` | Per-app |
| `tag` | `string` | Per-app |
| `digest` | `string` | `""` |
| `pullPolicy` | `string` | `"IfNotPresent"` |

If `digest` is set, it takes precedence over `tag`.

```yaml
spec:
  image:
    repository: lscr.io/linuxserver/sonarr
    tag: "4.0.2"
    pullPolicy: IfNotPresent
```

---

### `uid` / `gid`

**Type:** `int64` -- **Optional** -- **Default:** `65534`

User and group IDs for the container process. Used for PUID/PGID environment variables (LinuxServer images) or `runAsUser`/`runAsGroup` (NonRoot images), and for `fsGroup` on the pod security context.

```yaml
spec:
  uid: 1000
  gid: 1000
```

---

### `security`

**Type:** `SecurityProfile` -- **Optional**

Controls the container and pod security context. The `profileType` field selects one of three presets that determine how the remaining fields are applied.

| Sub-field | Type | Default |
|---|---|---|
| `profileType` | `SecurityProfileType` | `LinuxServer` |
| `user` | `int64` | `65534` |
| `group` | `int64` | `65534` |
| `runAsNonRoot` | `bool` | Derived from `profileType` |
| `readOnlyRootFilesystem` | `bool` | `false` |
| `allowPrivilegeEscalation` | `bool` | `false` |
| `capabilitiesAdd` | `[]string` | `[]` |
| `capabilitiesDrop` | `[]string` | `["ALL"]` for LinuxServer/NonRoot |

**Profile types:**

| Type | Behavior |
|---|---|
| `LinuxServer` | For s6-overlay images. Adds CHOWN, SETGID, SETUID capabilities. Sets PUID/PGID env vars and fsGroup from `user`/`group`. |
| `NonRoot` | For images that run as non-root natively. Sets `runAsUser`/`runAsGroup`/`fsGroup` from `user`/`group`. |
| `Custom` | Full manual control over all security context fields. |

```yaml
spec:
  security:
    profileType: NonRoot
    user: 1000
    group: 1000
    readOnlyRootFilesystem: true
    capabilitiesDrop:
      - ALL
```

---

### `service`

**Type:** `ServiceSpec` -- **Optional**

Configures the Kubernetes Service created for the app. When omitted, the operator creates a ClusterIP service on the app's default port.

| Sub-field | Type | Default |
|---|---|---|
| `serviceType` | `string` | `"ClusterIP"` |
| `ports` | `[]ServicePort` | Per-app defaults |

**ServicePort fields:**

| Field | Type | Default |
|---|---|---|
| `name` | `string` | -- |
| `port` | `int32` | -- |
| `protocol` | `string` | `"TCP"` |
| `containerPort` | `int32` | Same as `port` if omitted |
| `hostPort` | `int32` | -- |

```yaml
spec:
  service:
    serviceType: ClusterIP
    ports:
      - name: http
        port: 8989
      - name: metrics
        port: 9090
        protocol: TCP
```

---

### `gateway`

**Type:** `GatewaySpec` -- **Optional**

Configures Gateway API routing (HTTPRoute or TCPRoute) for external access.

| Sub-field | Type | Default |
|---|---|---|
| `enabled` | `bool` | `false` |
| `routeType` | `RouteType` | `Http` |
| `parentRefs` | `[]GatewayParentRef` | `[]` |
| `hosts` | `[]string` | `[]` |
| `tls` | `TlsSpec` | -- |

**TlsSpec fields:**

| Field | Type | Default |
|---|---|---|
| `enabled` | `bool` | `false` |
| `certIssuer` | `string` | `""` |
| `secretName` | `string` | Derived from app name |

When `tls.enabled` is true, the operator creates a cert-manager Certificate resource and switches the route type to TCPRoute for TLS pass-through.

**GatewayParentRef fields:**

| Field | Type | Default |
|---|---|---|
| `name` | `string` | -- |
| `namespace` | `string` | `""` |
| `sectionName` | `string` | `""` |

```yaml
spec:
  gateway:
    enabled: true
    routeType: Http
    parentRefs:
      - name: main-gateway
        namespace: gateway-system
    hosts:
      - sonarr.example.com
    tls:
      enabled: true
      certIssuer: letsencrypt-prod
```

---

### `resources`

**Type:** `ResourceRequirements` -- **Optional**

CPU and memory resource limits and requests for the container.

| Sub-field | Type | Default |
|---|---|---|
| `limits.cpu` | `string` | `"1"` |
| `limits.memory` | `string` | `"512Mi"` |
| `requests.cpu` | `string` | `"100m"` |
| `requests.memory` | `string` | `"128Mi"` |

```yaml
spec:
  resources:
    limits:
      cpu: "2"
      memory: 1Gi
    requests:
      cpu: 250m
      memory: 256Mi
```

---

### `persistence`

**Type:** `PersistenceSpec` -- **Optional**

Configures persistent storage. Supports PVC-backed volumes and NFS mounts.

| Sub-field | Type | Default |
|---|---|---|
| `volumes` | `[]PvcVolume` | Per-app defaults |
| `nfsMounts` | `[]NfsMount` | `[]` |

**PvcVolume fields:**

| Field | Type | Default |
|---|---|---|
| `name` | `string` | -- |
| `mountPath` | `string` | -- |
| `accessMode` | `string` | `"ReadWriteOnce"` |
| `size` | `string` | `"1Gi"` |
| `storageClass` | `string` | `""` (cluster default) |

**NfsMount fields:**

| Field | Type | Default |
|---|---|---|
| `name` | `string` | -- |
| `server` | `string` | -- |
| `path` | `string` | -- |
| `mountPath` | `string` | -- |
| `readOnly` | `bool` | `false` |

```yaml
spec:
  persistence:
    volumes:
      - name: config
        mountPath: /config
        size: 5Gi
        storageClass: longhorn
    nfsMounts:
      - name: media
        server: nas.local
        path: /volume1/media
        mountPath: /media
        readOnly: false
```

---

### `env`

**Type:** `[]EnvVar` -- **Optional** -- **Default:** `[{name: TZ, value: UTC}]`

Extra environment variables injected into the container. Each entry has `name` and `value` fields.

```yaml
spec:
  env:
    - name: TZ
      value: America/New_York
    - name: DOCKER_MODS
      value: ghcr.io/gilbn/theme.park:sonarr
```

---

### `probes`

**Type:** `ProbeSpec` -- **Optional**

Liveness and readiness probe configuration for the container.

| Sub-field | Type | Default |
|---|---|---|
| `liveness` | `ProbeConfig` | HTTP `/`, delay 30s |
| `readiness` | `ProbeConfig` | HTTP `/`, delay 10s |

**ProbeConfig fields:**

| Field | Type | Default |
|---|---|---|
| `probeType` | `ProbeType` | `Http` |
| `path` | `string` | `"/"` |
| `command` | `[]string` | `[]` (Exec probes only) |
| `initialDelaySeconds` | `int32` | `30` |
| `periodSeconds` | `int32` | `10` |
| `timeoutSeconds` | `int32` | `1` |
| `failureThreshold` | `int32` | `3` |

Valid `probeType` values: `Http`, `Tcp`, `Exec`

```yaml
spec:
  probes:
    liveness:
      probeType: Http
      path: /ping
      initialDelaySeconds: 60
      periodSeconds: 15
    readiness:
      probeType: Tcp
      initialDelaySeconds: 10
```

---

### `scheduling`

**Type:** `NodeScheduling` -- **Optional**

Controls pod placement via node selectors, tolerations, and affinity rules.

| Sub-field | Type | Default |
|---|---|---|
| `nodeSelector` | `map[string]string` | `{}` |
| `tolerations` | `[]object` | `[]` |
| `affinity` | `object` | -- |

Tolerations and affinity accept raw Kubernetes JSON objects matching the upstream API.

```yaml
spec:
  scheduling:
    nodeSelector:
      kubernetes.io/arch: amd64
    tolerations:
      - key: gpu
        operator: Equal
        value: "true"
        effect: NoSchedule
```

---

### `networkPolicy`

**Type:** `bool` -- **Optional**

Simple toggle to create a NetworkPolicy for the app. When `true`, the operator generates a basic ingress-only policy on the app's service ports.

For fine-grained control, use `networkPolicyConfig` instead (it takes precedence over this field).

```yaml
spec:
  networkPolicy: true
```

---

### `networkPolicyConfig`

**Type:** `NetworkPolicyConfig` -- **Optional**

Fine-grained NetworkPolicy configuration. When set, takes precedence over the boolean `networkPolicy` field.

| Sub-field | Type | Default |
|---|---|---|
| `allowSameNamespace` | `bool` | `true` |
| `allowDns` | `bool` | `true` |
| `allowInternetEgress` | `bool` | `false` |
| `deniedCidrBlocks` | `[]string` | `[]` |
| `customEgressRules` | `[]object` | `[]` |

`customEgressRules` accepts raw Kubernetes `NetworkPolicyEgressRule` JSON objects.

```yaml
spec:
  networkPolicyConfig:
    allowSameNamespace: true
    allowDns: true
    allowInternetEgress: false
    deniedCidrBlocks:
      - 10.0.0.0/8
      - 172.16.0.0/12
      - 192.168.0.0/16
```

---

### `appConfig`

**Type:** `AppConfig` (enum) -- **Optional**

App-specific configuration. This is a tagged enum -- use the variant name matching your app type as the key.

#### Variant: `Transmission`

| Sub-field | Type | Default |
|---|---|---|
| `settings` | `object` | `{}` |
| `peerPort` | `PeerPortConfig` | -- |
| `auth` | `TransmissionAuth` | -- |

**PeerPortConfig fields:**

| Field | Type | Default |
|---|---|---|
| `port` | `int32` | -- |
| `hostPort` | `bool` | `false` |
| `randomOnStart` | `bool` | `false` |
| `randomLow` | `int32` | `49152` |
| `randomHigh` | `int32` | `65535` |

**TransmissionAuth fields:**

| Field | Type |
|---|---|
| `secretName` | `string` |

```yaml
spec:
  appConfig:
    Transmission:
      settings:
        download-dir: /downloads/complete
        incomplete-dir: /downloads/incomplete
      peerPort:
        port: 51413
        hostPort: true
      auth:
        secretName: transmission-credentials
```

#### Variant: `Sabnzbd`

| Sub-field | Type | Default |
|---|---|---|
| `hostWhitelist` | `[]string` | `[]` |
| `tarUnpack` | `bool` | `false` |

`hostWhitelist` lists hostnames SABnzbd should accept connections from (required for reverse proxy setups). `tarUnpack` installs compression tools and adds a post-processing script for automatic archive unpacking.

```yaml
spec:
  appConfig:
    Sabnzbd:
      hostWhitelist:
        - sabnzbd.example.com
      tarUnpack: true
```

#### Variant: `Prowlarr`

| Sub-field | Type | Default |
|---|---|---|
| `customDefinitions` | `[]IndexerDefinition` | `[]` |

Each definition creates a YAML file at `/config/Definitions/Custom/{name}.yml` inside the Prowlarr container.

**IndexerDefinition fields:**

| Field | Type |
|---|---|
| `name` | `string` |
| `content` | `string` |

```yaml
spec:
  appConfig:
    Prowlarr:
      customDefinitions:
        - name: my-private-tracker
          content: |
            id: my-private-tracker
            name: My Private Tracker
            ...
```

#### Variant: `Overseerr`

| Sub-field | Type | Default |
|---|---|---|
| `sonarr` | `OverseerrServerDefaults` | -- |
| `radarr` | `OverseerrServerDefaults` | -- |

**OverseerrServerDefaults fields:**

| Field | Type | Default |
|---|---|---|
| `profileId` | `float64` | -- |
| `profileName` | `string` | -- |
| `rootFolder` | `string` | -- |
| `minimumAvailability` | `string` | -- (Radarr only) |
| `enableSeasonFolders` | `bool` | -- (Sonarr only) |
| `fourK` | `OverseerrServerDefaults4k` | -- |

The `fourK` sub-object has the same fields as `OverseerrServerDefaults` and is used for 4K instances.

```yaml
spec:
  appConfig:
    Overseerr:
      sonarr:
        profileId: 1
        profileName: "HD-1080p"
        rootFolder: "/tv"
        enableSeasonFolders: true
        fourK:
          profileId: 5
          profileName: "Ultra-HD"
          rootFolder: "/tv4k"
          enableSeasonFolders: true
      radarr:
        profileId: 1
        profileName: "HD-1080p"
        rootFolder: "/movies"
        minimumAvailability: "released"
        fourK:
          profileId: 5
          profileName: "Ultra-HD"
          rootFolder: "/movies4k"
          minimumAvailability: "released"
```

---

### `apiKeySecret`

**Type:** `string` -- **Optional**

Name of a Kubernetes Secret in the same namespace containing an `api-key` data field. The operator reads the API key from this Secret for API health checks and backup operations.

```yaml
spec:
  apiKeySecret: sonarr-api-key
```

The referenced Secret should look like:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: sonarr-api-key
type: Opaque
stringData:
  api-key: "your-api-key-here"
```

---

### `apiHealthCheck`

**Type:** `ApiHealthCheckSpec` -- **Optional**

Enables API-level health checking that hits the application's API endpoint (rather than just an HTTP probe). Requires `apiKeySecret` to be set.

| Sub-field | Type | Default |
|---|---|---|
| `enabled` | `bool` | `false` |
| `intervalSeconds` | `uint32` | `60` (when omitted) |

```yaml
spec:
  apiKeySecret: sonarr-api-key
  apiHealthCheck:
    enabled: true
    intervalSeconds: 30
```

---

### `backup`

**Type:** `BackupSpec` -- **Optional**

Configures automated backups via the application's API. Only supported for Servarr v3 apps: Sonarr, Radarr, Lidarr, and Prowlarr. Requires `apiKeySecret` to be set.

| Sub-field | Type | Default |
|---|---|---|
| `enabled` | `bool` | `false` |
| `schedule` | `string` | `""` |
| `retentionCount` | `uint32` | `5` |

The `schedule` field accepts a standard cron expression.

```yaml
spec:
  apiKeySecret: sonarr-api-key
  backup:
    enabled: true
    schedule: "0 3 * * *"
    retentionCount: 7
```

---

### `imagePullSecrets`

**Type:** `[]string` -- **Optional**

Names of Kubernetes Secrets for private container registry authentication. Each name references a Secret of type `kubernetes.io/dockerconfigjson` in the same namespace.

```yaml
spec:
  imagePullSecrets:
    - ghcr-credentials
    - docker-hub-credentials
```

---

### `podAnnotations`

**Type:** `map[string]string` -- **Optional**

Additional annotations added to the pod template in the generated Deployment. Useful for integrations like Prometheus scraping, service meshes, or backup tools.

```yaml
spec:
  podAnnotations:
    prometheus.io/scrape: "true"
    prometheus.io/port: "9090"
```

---

### `gpu`

**Type:** `GpuSpec` -- **Optional**

GPU device passthrough for hardware-accelerated transcoding. When set, the operator adds the corresponding device plugin resource to the container's resource limits and requests.

| Sub-field | Type | Resource Added |
|---|---|---|
| `nvidia` | `int32` | `nvidia.com/gpu` |
| `intel` | `int32` | `gpu.intel.com/i915` |
| `amd` | `int32` | `amd.com/gpu` |

Each field specifies the count of GPU devices to request.

```yaml
spec:
  gpu:
    intel: 1
```

```yaml
spec:
  gpu:
    nvidia: 1
```

---

### `prowlarrSync`

**Type:** `ProwlarrSyncSpec` -- **Optional**

Configures Prowlarr cross-app synchronization. Only applies to `Prowlarr`-type apps. When enabled, the operator discovers Sonarr, Radarr, and Lidarr instances in the target namespace and registers them as applications in Prowlarr for indexer sync.

| Sub-field | Type | Default |
|---|---|---|
| `enabled` | `bool` | `false` |
| `namespaceScope` | `string` | Same namespace as the Prowlarr CR |
| `autoRemove` | `bool` | `true` |

When `autoRemove` is true, apps are removed from Prowlarr when their corresponding ServarrApp CRs are deleted.

```yaml
spec:
  app: Prowlarr
  apiKeySecret: prowlarr-api-key
  prowlarrSync:
    enabled: true
    autoRemove: true
```

---

### `overseerrSync`

**Type:** `OverseerrSyncSpec` -- **Optional**

Configures Overseerr cross-app synchronization. Only applies to `Overseerr`-type apps. When enabled, the operator discovers Sonarr and Radarr instances in the target namespace and registers them as servers in Overseerr with correct `is4k`/`isDefault` flags.

| Sub-field | Type | Default |
|---|---|---|
| `enabled` | `bool` | `false` |
| `namespaceScope` | `string` | Same namespace as the Overseerr CR |
| `autoRemove` | `bool` | `true` |

When `autoRemove` is true, servers are removed from Overseerr when their corresponding ServarrApp CRs are deleted.

```yaml
spec:
  app: Overseerr
  apiKeySecret: overseerr-api-key
  overseerrSync:
    enabled: true
    autoRemove: true
```

---

## MediaStack-Specific Fields

These fields are available on `StackApp` entries within a `MediaStack` spec, but not on standalone `ServarrApp` resources.

### `split4k`

**Type:** `bool` -- **Optional** -- **Default:** `false`

When true, the MediaStack controller creates both a standard and a 4K instance of this app. Only valid for Sonarr and Radarr. The 4K instance is created with `instance: "4k"` and inherits all fields from the parent StackApp entry.

```yaml
apiVersion: servarr.dev/v1alpha1
kind: MediaStack
metadata:
  name: media
spec:
  apps:
    - app: Sonarr
      split4k: true
      apiKeySecret: sonarr-api-key
```

This produces two child ServarrApp resources: `media-sonarr` and `media-sonarr-4k`.

### `split4kOverrides`

**Type:** `Split4kOverrides` -- **Optional**

Override fields applied only to the 4K instance when `split4k` is true.

| Sub-field | Type |
|---|---|
| `image` | `ImageSpec` |
| `resources` | `ResourceRequirements` |
| `persistence` | `PersistenceSpec` |
| `env` | `[]EnvVar` |
| `service` | `ServiceSpec` |
| `gateway` | `GatewaySpec` |

```yaml
spec:
  apps:
    - app: Radarr
      split4k: true
      split4kOverrides:
        resources:
          limits:
            cpu: "4"
            memory: 2Gi
        env:
          - name: CUSTOM_4K_SETTING
            value: "true"
```

---

## Full Example

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
  namespace: media
spec:
  app: Sonarr
  uid: 1000
  gid: 1000
  resources:
    limits:
      cpu: "2"
      memory: 1Gi
    requests:
      cpu: 250m
      memory: 256Mi
  persistence:
    volumes:
      - name: config
        mountPath: /config
        size: 5Gi
        storageClass: longhorn
    nfsMounts:
      - name: media
        server: nas.local
        path: /volume1/media
        mountPath: /media
  gateway:
    enabled: true
    parentRefs:
      - name: main-gateway
        namespace: gateway-system
    hosts:
      - sonarr.example.com
    tls:
      enabled: true
      certIssuer: letsencrypt-prod
  env:
    - name: TZ
      value: America/New_York
  apiKeySecret: sonarr-api-key
  apiHealthCheck:
    enabled: true
    intervalSeconds: 30
  backup:
    enabled: true
    schedule: "0 3 * * *"
    retentionCount: 7
  networkPolicyConfig:
    allowSameNamespace: true
    allowDns: true
    allowInternetEgress: false
```
