# Troubleshooting

Common issues with the Servarr Operator, organized by symptom with diagnosis steps and fixes.

---

## 1. Operator Not Starting

### Symptom

The `servarr-operator` pod is in `CrashLoopBackOff` or `Error` state, or never reaches `Running`.

### Diagnosis

**Check pod status and logs:**

```bash
kubectl get pods -l app.kubernetes.io/name=servarr-operator
kubectl logs deploy/servarr-operator --previous
```

**Check RBAC resources:**

```bash
kubectl get clusterrole | grep servarr
kubectl get clusterrolebinding | grep servarr
kubectl get sa -n <operator-namespace>
```

Look for errors like `is forbidden: User "system:serviceaccount:..." cannot` in the pod logs.

**Check if the CRD is installed:**

```bash
kubectl get crd servarrapps.servarr.dev
```

**Check for image pull errors:**

```bash
kubectl describe pod -l app.kubernetes.io/name=servarr-operator | grep -A5 Events
```

### Fix

**Missing ClusterRole or ClusterRoleBinding:** The operator needs permissions to manage Deployments, Services, PVCs, ConfigMaps, NetworkPolicies, and ServarrApp CRs across namespaces. Ensure the ClusterRole grants at least:

- `servarrapps.servarr.dev` -- all verbs
- `deployments.apps`, `services`, `configmaps`, `persistentvolumeclaims` -- get, list, watch, create, patch, update, delete
- `networkpolicies.networking.k8s.io` -- get, list, watch, create, patch, update, delete
- `events` -- create, patch
- `secrets` -- get (for API key reading)
- `customresourcedefinitions.apiextensions.k8s.io` -- get, create, patch (for CRD self-registration)

Verify the ClusterRoleBinding references the correct ServiceAccount name and namespace.

**CRD not installed:** The operator auto-registers the CRD on startup, but if RBAC blocks that, install it manually:

```bash
servarr-operator crd | kubectl apply -f -
```

**Image pull errors:** Verify the image exists and pull secrets are configured on the operator Deployment (not the ServarrApp CR). For private registries, ensure an `imagePullSecrets` entry exists on the operator pod spec.

---

## 2. App Stuck in Not-Ready State

### Symptom

A `ServarrApp` resource shows `Ready: false` indefinitely. The managed Deployment exists but has 0 ready replicas.

```bash
kubectl get sa
# NAME      APP        READY
# sonarr    Sonarr     false
```

### Diagnosis

**Check the Deployment and pod status:**

```bash
kubectl get deploy -l app.kubernetes.io/managed-by=servarr-operator
kubectl get pods -l app.kubernetes.io/managed-by=servarr-operator
kubectl describe pod <pod-name>
```

**Check ServarrApp conditions:**

```bash
kubectl get sa <name> -o jsonpath='{.status.conditions}' | jq .
```

Look for `DeploymentReady: False` and `Degraded: True` conditions.

**Check probe failures:**

```bash
kubectl logs <pod-name>
kubectl describe pod <pod-name> | grep -A3 "Liveness\|Readiness"
```

**Check PVC binding:**

```bash
kubectl get pvc -l app.kubernetes.io/managed-by=servarr-operator
```

### Fix

**Probe misconfiguration:** The default probes are HTTP GET on `/` with a 30-second initial delay (liveness) and 10-second initial delay (readiness), period of 10s/5s respectively, 1s timeout, and 3 failure threshold. Some apps have slow startup or different health endpoints. Override in the CR:

```yaml
spec:
  probes:
    liveness:
      path: "/ping"
      initialDelaySeconds: 60
      periodSeconds: 10
    readiness:
      path: "/ping"
      initialDelaySeconds: 15
      periodSeconds: 5
```

For apps behind authentication (like Transmission with `auth` configured), the operator automatically switches to exec-based probes using `curl` with credentials. If you override probes manually, ensure they account for auth.

**Image pull errors:** Check if the image repository and tag are correct. If using a private registry, set `imagePullSecrets` on the ServarrApp CR:

```yaml
spec:
  imagePullSecrets:
    - my-registry-secret
```

Verify the secret exists:

```bash
kubectl get secret my-registry-secret
```

**PVC not bound:** PVCs stay `Pending` when the StorageClass does not exist or has no available capacity.

```bash
kubectl get pvc -l app.kubernetes.io/instance=<name>
kubectl describe pvc <name>-config
kubectl get storageclass
```

Either set a valid `storageClass` on the persistence volume or ensure the cluster default StorageClass is configured:

```yaml
spec:
  persistence:
    volumes:
      - name: config
        mountPath: /config
        size: 5Gi
        storageClass: local-path
```

---

## 3. Drift Detection Keeps Correcting

### Symptom

Operator logs repeatedly show `deployment drift detected, re-applying`. Events on the ServarrApp show `DriftDetected` warnings.

```bash
kubectl get events --field-selector reason=DriftDetected
```

### Diagnosis

**Check what is modifying the resources:**

```bash
kubectl get deploy <name> -o jsonpath='{.metadata.managedFields}' | jq '.[].manager'
```

If you see managers other than `servarr-operator`, something else is editing the resources.

**Check operator events:**

```bash
kubectl describe sa <name> | grep -A2 DriftDetected
```

### Fix

The operator reconciles every 5 minutes and uses server-side apply to enforce the desired state. Any manual edits to operator-managed resources (Deployment, Service, PVC, NetworkPolicy, ConfigMap, HTTPRoute) are overwritten on the next reconcile cycle.

Do not edit these resources directly. Instead, edit the ServarrApp CR:

```bash
kubectl edit sa <name>
```

Common fields people try to change on the Deployment directly but should change on the CR:

| Want to change        | Edit in the CR under          |
|-----------------------|-------------------------------|
| Image tag             | `spec.image.tag`              |
| Resource limits       | `spec.resources.limits`       |
| Environment variables | `spec.env`                    |
| Replicas              | Not configurable (always 1)   |
| Volume mounts         | `spec.persistence`            |
| Security context      | `spec.security`               |

---

## 4. Gateway Routes Not Working

### Symptom

The ServarrApp has `gateway.enabled: true` but the app is not reachable via the expected hostname. No HTTPRoute or TCPRoute appears, or it exists but traffic does not reach the app.

### Diagnosis

**Check if Gateway API CRDs are installed:**

```bash
kubectl get crd httproutes.gateway.networking.k8s.io
kubectl get crd tcproutes.gateway.networking.k8s.io
kubectl get crd gateways.gateway.networking.k8s.io
```

**Check if the route was created:**

```bash
kubectl get httproute -l app.kubernetes.io/managed-by=servarr-operator
kubectl get tcproute -l app.kubernetes.io/managed-by=servarr-operator
```

**Check operator logs for route apply errors:**

```bash
kubectl logs deploy/servarr-operator | grep -i "route\|gateway\|HTTPRoute\|TCPRoute"
```

**Check the route status for accepted/attached conditions:**

```bash
kubectl get httproute <name> -o yaml
```

Look at `.status.parents[].conditions` for `Accepted: False` or `ResolvedRefs: False`.

**Check the Gateway itself:**

```bash
kubectl get gateway -A
kubectl describe gateway <gateway-name> -n <gateway-namespace>
```

### Fix

**Missing Gateway API CRDs:** Install the Gateway API CRDs before creating routes. The operator uses `gateway.networking.k8s.io/v1` for HTTPRoute and `gateway.networking.k8s.io/v1alpha2` for TCPRoute:

```bash
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/latest/download/standard-install.yaml
```

**parentRefs misconfigured:** The `parentRefs` must reference an existing Gateway by both `name` and `namespace`. If the Gateway is in a different namespace than the ServarrApp, both fields are required:

```yaml
spec:
  gateway:
    enabled: true
    parentRefs:
      - name: my-gateway
        namespace: gateway-system
    hosts:
      - sonarr.example.com
```

If `namespace` is empty, the route assumes the Gateway is in the same namespace as the ServarrApp. Verify the Gateway exists at the referenced name and namespace.

**Hosts not matching:** The `hosts` array must contain hostnames that the Gateway listener is configured to accept. Check the Gateway's listener `hostname` or `allowedRoutes` configuration. If the Gateway uses a wildcard like `*.example.com`, the route host must be within that domain.

---

## 5. Backup Failures

### Symptom

The ServarrApp status shows backup errors, or backups never run despite being configured.

```bash
kubectl get sa <name> -o jsonpath='{.status.backupStatus}' | jq .
```

Output shows `last_backup_result` containing an error message, or the field is absent entirely.

### Diagnosis

**Check if backup is configured correctly:**

```bash
kubectl get sa <name> -o jsonpath='{.spec.backup}' | jq .
```

Ensure `enabled: true` and `schedule` is a valid cron expression.

**Check if apiKeySecret is set:**

```bash
kubectl get sa <name> -o jsonpath='{.spec.apiKeySecret}'
```

**Verify the secret exists and contains the `api-key` field:**

```bash
kubectl get secret <secret-name>
kubectl get secret <secret-name> -o jsonpath='{.data.api-key}' | base64 -d
```

**Check operator logs for backup errors:**

```bash
kubectl logs deploy/servarr-operator | grep -i backup
```

**Verify the app API is reachable from the operator:**

```bash
kubectl run curl-test --rm -it --image=curlimages/curl -- \
  curl -s -H "X-Api-Key: <api-key>" http://<name>.<namespace>.svc:<port>/api/v3/system/backup
```

### Fix

**Missing apiKeySecret:** The backup feature requires `apiKeySecret` to be set on the CR. Create the secret and reference it:

```bash
kubectl create secret generic sonarr-api-key --from-literal=api-key=<your-api-key>
```

```yaml
spec:
  apiKeySecret: sonarr-api-key
  backup:
    enabled: true
    schedule: "0 3 * * *"
    retentionCount: 5
```

**App API not reachable:** Backups call the app via its in-cluster Service at `http://<name>.<namespace>.svc:<port>`. Verify the Service exists and the pod is running. DNS resolution issues can occur if CoreDNS is unhealthy or the NetworkPolicy blocks egress from the operator.

**App not a Servarr v3 type:** Only Sonarr, Radarr, Lidarr, and Prowlarr support the backup API (Servarr v3 `/api/v3/system/backup`). Apps like Transmission, Sabnzbd, Tautulli, Overseerr, Maintainerr, Jackett, and Jellyfin do not support operator-managed backups. The operator silently ignores backup config for non-v3 app types.

---

## 6. NetworkPolicy Blocking Traffic

### Symptom

Pods managed by the operator cannot reach DNS, other pods in the namespace, or external services. The app itself works when NetworkPolicy is disabled but fails when enabled.

### Diagnosis

**Check if a NetworkPolicy exists for the app:**

```bash
kubectl get netpol -l app.kubernetes.io/managed-by=servarr-operator
kubectl describe netpol <name>
```

**Check the NetworkPolicy config on the CR:**

```bash
kubectl get sa <name> -o jsonpath='{.spec.networkPolicyConfig}' | jq .
```

**Test DNS from the pod:**

```bash
kubectl exec <pod-name> -- nslookup google.com
```

**Check if same-namespace communication works:**

```bash
kubectl exec <pod-name> -- curl -s http://<other-service>.<namespace>.svc:<port>/
```

### Fix

**DNS egress not enabled:** The `allowDns` field defaults to `true` when using `networkPolicyConfig`, which permits UDP port 53 egress to pods labeled `k8s-app=kube-dns` in any namespace. If you have explicitly set it to false, or if your DNS pods use different labels, DNS resolution will fail:

```yaml
spec:
  networkPolicyConfig:
    allowDns: true
```

If your cluster DNS pods do not carry the `k8s-app=kube-dns` label (some distributions use different labels), add a custom egress rule targeting your DNS pods.

**Same-namespace ingress rules:** The `allowSameNamespace` field (defaults to `true`) permits ingress from any pod in the same namespace on the app's service ports. If disabled, other apps in the namespace (including Prowlarr sync) cannot reach this app:

```yaml
spec:
  networkPolicyConfig:
    allowSameNamespace: true
```

**Egress to external services blocked:** By default, `allowInternetEgress` is `false`. Apps that need to reach external APIs (indexers, download clients, metadata providers) require this to be enabled. When enabled, the operator blocks RFC 1918 ranges (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`) by default to prevent lateral movement:

```yaml
spec:
  networkPolicyConfig:
    allowInternetEgress: true
    deniedCidrBlocks:
      - "10.0.0.0/8"
      - "172.16.0.0/12"
      - "192.168.0.0/16"
```

**Gateway namespace ingress:** When `gateway.enabled` is true and `parentRefs` have a `namespace` set, the operator automatically adds an ingress rule allowing traffic from that namespace. If the gateway namespace is missing from `parentRefs`, the ingress rule will not be generated and gateway traffic will be blocked.

---

## 7. Prowlarr Sync Not Registering Apps

### Symptom

Prowlarr has `prowlarrSync.enabled: true` but discovered apps do not appear in Prowlarr's Settings > Apps page. Operator logs show `ProwlarrSyncComplete` with 0 apps synced.

### Diagnosis

**Check the Prowlarr sync config:**

```bash
kubectl get sa <prowlarr-name> -o jsonpath='{.spec.prowlarrSync}' | jq .
```

**Check operator logs for sync details:**

```bash
kubectl logs deploy/servarr-operator | grep -i prowlarr
```

Look for `skipping app: failed to read API key` messages.

**List all ServarrApps in the target namespace:**

```bash
kubectl get sa -n <target-namespace>
```

**Check which apps have apiKeySecret set:**

```bash
kubectl get sa -n <target-namespace> -o custom-columns='NAME:.metadata.name,APP:.spec.app,API_KEY_SECRET:.spec.apiKeySecret'
```

### Fix

**Apps in a different namespace:** By default, Prowlarr sync discovers apps in the same namespace as the Prowlarr CR. If apps are in another namespace, set `namespaceScope`:

```yaml
spec:
  prowlarrSync:
    enabled: true
    namespaceScope: media-apps
```

The operator must have RBAC permissions to list ServarrApps and read Secrets in the target namespace.

**Apps missing apiKeySecret:** Prowlarr sync only registers apps that have `apiKeySecret` configured. Apps without it are silently skipped because the operator cannot provide the API key to Prowlarr. Set `apiKeySecret` on each Sonarr/Radarr/Lidarr CR:

```yaml
spec:
  apiKeySecret: sonarr-api-key
```

The secret must contain an `api-key` data field:

```bash
kubectl create secret generic sonarr-api-key --from-literal=api-key=<key>
```

**Only Servarr v3 apps are discovered:** The sync only discovers Sonarr, Radarr, and Lidarr instances. Other app types (Sabnzbd, Transmission, Jellyfin, etc.) are not registered in Prowlarr.

**Prowlarr itself needs apiKeySecret:** The Prowlarr CR also requires `apiKeySecret` to authenticate against its own API for managing applications:

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: prowlarr
spec:
  app: Prowlarr
  apiKeySecret: prowlarr-api-key
  prowlarrSync:
    enabled: true
```

---

## 8. Pod Keeps Restarting

### Symptom

The app pod enters a `CrashLoopBackOff` cycle or is repeatedly killed and restarted. The Deployment shows a high restart count.

```bash
kubectl get pods -l app.kubernetes.io/instance=<name>
# NAME            READY   STATUS             RESTARTS
# sonarr-abc123   0/1     CrashLoopBackOff   12
```

### Diagnosis

**Check the pod termination reason:**

```bash
kubectl describe pod <pod-name> | grep -A5 "Last State\|State:"
```

Look for `OOMKilled` (out of memory) or `Error` (exit code non-zero).

**Check resource usage vs limits:**

```bash
kubectl top pod <pod-name>
kubectl get sa <name> -o jsonpath='{.spec.resources}' | jq .
```

**Check liveness probe timing:**

```bash
kubectl describe pod <pod-name> | grep -A3 Liveness
```

If the liveness probe fails during startup, Kubernetes kills the container before it finishes initializing.

**Check container logs from the previous crash:**

```bash
kubectl logs <pod-name> --previous
```

### Fix

**Resource limits too low:** The default resource limits are 1 CPU / 512Mi memory (requests: 100m CPU / 128Mi memory). Apps doing heavy indexing, library scans, or transcoding (Jellyfin) may need more:

```yaml
spec:
  resources:
    limits:
      cpu: "2"
      memory: "2Gi"
    requests:
      cpu: "250m"
      memory: "512Mi"
```

If the pod was `OOMKilled`, increase the memory limit. Check `kubectl describe pod` for the `reason: OOMKilled` indicator.

**Liveness probe timing too aggressive:** If the app takes longer than `initialDelaySeconds + (failureThreshold * periodSeconds)` to become healthy, the liveness probe kills it. With defaults (30s delay, 10s period, 3 failures), the pod has ~60 seconds to start responding. For slow-starting apps or first-run database migrations, increase the initial delay:

```yaml
spec:
  probes:
    liveness:
      initialDelaySeconds: 120
      periodSeconds: 10
      failureThreshold: 5
    readiness:
      initialDelaySeconds: 30
      periodSeconds: 5
```

Alternatively, switch the liveness probe to TCP if the HTTP endpoint is not available during initialization:

```yaml
spec:
  probes:
    liveness:
      probeType: Tcp
      initialDelaySeconds: 60
```

**Application-level crash:** If the container exits with a non-zero code and logs show application errors (database corruption, permission denied on config volume), the issue is not with the operator. Check file ownership matches the configured UID/GID (default: 65534/65534 for both LinuxServer and NonRoot profiles) and that the PVC has sufficient space.
