# Kora Helm Chart

**A Confluent-compatible Schema Registry, built in Rust.** See the [main README](../README.md) for project details.

## Prerequisites

- Kubernetes 1.24+
- Helm 3.8+
- An existing PostgreSQL database

## Quick Start

```bash
helm install kora oci://ghcr.io/Popsink/kora/charts/kora \
  --set database.host=my-postgres.example.com \
  --set database.password=secret
```

## Database Configuration

Two channels:

- **Component mode** (default) — `host` / `port` / `user` / `database` plain in values, `password` always mounted via `secretKeyRef` (auto-Secret or `existingSecret`).
- **URL mode** — full `DATABASE_URL` mounted via `secretKeyRef` (auto-Secret or `existingSecret`). Triggered by setting `database.url` or `database.secretKeys.url` (with `existingSecret`). Components are ignored.

The auto-Secret base64-encodes the value but does not encrypt it — never commit `helm template` output to git.

### Component mode — inline

```bash
helm install kora oci://ghcr.io/Popsink/kora/charts/kora \
  --set database.host=postgres.example.com \
  --set database.password=secret
```

### Component mode — password from existing Secret

Suits ExternalSecrets Operator, CloudNativePG, AWS RDS via ESO, Vault, etc.

```bash
helm install kora oci://ghcr.io/Popsink/kora/charts/kora \
  --set database.host=postgres.example.com \
  --set database.existingSecret=kora-db-credentials
```

Override the key with `--set database.secretKeys.password=db-password` if it isn't `password`.

### URL mode — inline

```bash
helm install kora oci://ghcr.io/Popsink/kora/charts/kora \
  --set database.url="postgres://kora:secret@pg:5432/kora?sslmode=require"
```

### URL mode — from existing Secret

```bash
kubectl create secret generic kora-db \
  --from-literal=DATABASE_URL="postgres://kora:secret@pg:5432/kora?sslmode=require"

helm install kora oci://ghcr.io/Popsink/kora/charts/kora \
  --set database.existingSecret=kora-db \
  --set database.secretKeys.url=DATABASE_URL
```

## Production Example

```yaml
replicaCount: 3

database:
  host: postgres.example.com
  existingSecret: kora-db-credentials

ingress:
  enabled: true
  hostname: schema-registry.example.com
  tls: true
  ingressClassName: nginx
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt

autoscaling:
  enabled: true
  minReplicas: 2
  maxReplicas: 10

pdb:
  create: true

metrics:
  enabled: true
  serviceMonitor:
    enabled: true

networkPolicy:
  enabled: true

resources:
  requests:
    cpu: 250m
    memory: 128Mi
  limits:
    cpu: "1"
    memory: 512Mi
```

## Parameters

### Image

| Parameter | Default | Description |
|---|---|---|
| `image.registry` | `ghcr.io` | Image registry |
| `image.repository` | `Popsink/kora` | Image repository |
| `image.tag` | `v{appVersion}` | Image tag |
| `image.digest` | `""` | Image digest (overrides tag) |
| `image.pullPolicy` | `IfNotPresent` | Pull policy |

### Kora Configuration

| Parameter | Default | Description |
|---|---|---|
| `kora.port` | `8080` | Listen port |
| `kora.logLevel` | `info` | Log level (RUST_LOG) |
| `kora.dbPoolMax` | `20` | Max DB connections |
| `kora.maxBodySize` | `16777216` | Max request body (bytes) |
| `kora.extraEnvVars` | `[]` | Extra env vars |
| `kora.extraEnvVarsCM` | `""` | ConfigMap with extra env vars |
| `kora.extraEnvVarsSecret` | `""` | Secret with extra env vars |

### Database

| Parameter | Default | Description |
|---|---|---|
| `database.host` | `""` | PostgreSQL host (plain) |
| `database.port` | `5432` | PostgreSQL port (plain) |
| `database.user` | `kora` | PostgreSQL user (plain) |
| `database.database` | `kora` | PostgreSQL database name (plain) |
| `database.password` | `""` | Direct password (component mode; goes into the auto-Secret) |
| `database.url` | `""` | Direct DATABASE_URL (URL mode; goes into the auto-Secret) |
| `database.existingSecret` | `""` | Existing Secret holding the password or the URL |
| `database.secretKeys.password` | `password` | Key in existingSecret holding the password |
| `database.secretKeys.url` | `""` | Key in existingSecret holding the DATABASE_URL — when set, switches to URL mode |

### Deployment

| Parameter | Default | Description |
|---|---|---|
| `replicaCount` | `1` | Number of replicas |
| `updateStrategy.type` | `RollingUpdate` | Deployment strategy |
| `terminationGracePeriodSeconds` | `30` | Grace period |
| `initContainers` | `[]` | Init containers |
| `sidecars` | `[]` | Sidecar containers |
| `priorityClassName` | `""` | Priority class |
| `schedulerName` | `""` | Scheduler name |

### Resources

| Parameter | Default | Description |
|---|---|---|
| `resources.requests.cpu` | `100m` | CPU request |
| `resources.requests.memory` | `64Mi` | Memory request |
| `resources.limits.cpu` | `500m` | CPU limit |
| `resources.limits.memory` | `256Mi` | Memory limit |

### Service

| Parameter | Default | Description |
|---|---|---|
| `service.type` | `ClusterIP` | Service type |
| `service.ports.http` | `8080` | HTTP port |
| `service.annotations` | `{}` | Service annotations |

### Service Account

| Parameter | Default | Description |
|---|---|---|
| `serviceAccount.create` | `true` | Create ServiceAccount |
| `serviceAccount.annotations` | `{}` | SA annotations (IRSA, Workload Identity) |
| `serviceAccount.automountServiceAccountToken` | `false` | Mount API token |

### Ingress

| Parameter | Default | Description |
|---|---|---|
| `ingress.enabled` | `false` | Enable ingress |
| `ingress.hostname` | `kora.local` | Hostname |
| `ingress.tls` | `false` | Enable TLS |
| `ingress.selfSigned` | `false` | Self-signed TLS cert |
| `ingress.ingressClassName` | `""` | Ingress class |
| `ingress.annotations` | `{}` | Ingress annotations |
| `ingress.extraHosts` | `[]` | Additional hosts |
| `ingress.extraTls` | `[]` | Additional TLS config |
| `ingress.secrets` | `[]` | Custom TLS secrets |

### Autoscaling

| Parameter | Default | Description |
|---|---|---|
| `autoscaling.enabled` | `false` | Enable HPA |
| `autoscaling.minReplicas` | `2` | Min replicas |
| `autoscaling.maxReplicas` | `10` | Max replicas |
| `autoscaling.targetCPU` | `80` | Target CPU % |

### PDB / NetworkPolicy / Metrics

| Parameter | Default | Description |
|---|---|---|
| `pdb.create` | `false` | Enable PodDisruptionBudget |
| `pdb.maxUnavailable` | `1` | Max unavailable pods |
| `networkPolicy.enabled` | `false` | Enable NetworkPolicy |
| `metrics.enabled` | `false` | Enable metrics |
| `metrics.serviceMonitor.enabled` | `false` | Enable ServiceMonitor |
| `metrics.serviceMonitor.interval` | `30s` | Scrape interval |

All probes (startup, readiness, liveness) are enabled by default with sensible timings. Override via `custom{Startup,Readiness,Liveness}Probe`. Lifecycle hooks, security contexts, scheduling, and annotations are fully configurable — see [`values.yaml`](values.yaml) for the complete reference.

## Security

PSS "restricted" by default: non-root (UID 65534), read-only root filesystem, no privilege escalation, all capabilities dropped, seccomp RuntimeDefault.

## Uninstalling

```bash
helm uninstall kora
```
