# Load Tests

k6 load tests for Kora schema registry.

## Prerequisites

- [k6](https://grafana.com/docs/k6/latest/set-up/install-k6/) installed (`brew install k6`)
- Docker
- Rust toolchain

## Quick start

```bash
just smoke
```

Every recipe automatically:
1. Starts a dedicated PostgreSQL (port 5433, `pg_stat_statements`)
2. Builds Kora and starts it against that DB
3. Runs the k6 scenario (scenarios only hit Kora's HTTP API)
4. Kills Kora when done

Override pool size: `DB_POOL_MAX=50 just stress`.

> `just loadtest-stop` tears down the load-test infrastructure and wipes the volumes.

## Scenarios

| Recipe | Scenario | VUs | Duration | Purpose |
|---|---|---|---|---|
| `just smoke` | smoke.js | 1 | 30s | Baseline — full user journey, establish latency floor |
| `just load` | load.js | 50+ readers, 20 writes/s, 5 compat, 3 check | 5 min | Nominal production load |
| `just stress` | stress.js | 10 → 300 (ramp) | 6 min | Find the breaking point, pool saturation |
| `just soak` | soak.js | 30 | 2h | Query degradation, dead tuples, table bloat |
| `just contention` | contention.js | 10 → 50 (ramp) | 5 min | FOR UPDATE lock, MAX(version)+1, TOCTOU |
| `just delete-load` | delete-under-load.js | 10 writers + 5 deleters + 5 readers | 3 min | Delete race conditions, reference protection |

## Running against the QA environment

The recipes above build Kora and run it natively against a local PostgreSQL. To
instead drive a **deployed** Kora — behind Traefik, with the basic auth production
uses — use the `qa-*` recipes. They need no local k6 (it runs in Docker) and no
`cargo build`.

The environment itself lives in the `data-plane` repo:

```bash
cd ../data-plane
PPSK_TARGET=qa inv up          # Kind cluster, Kora from its Helm chart, Traefik, Grafana
```

It exposes **the same Kora pod** through two routes, so running a scenario against
both attributes the difference to the ingress:

| Target | URL | Auth |
|---|---|---|
| `auth` (default) | `https://kora.ppsk.localhost:8443` | Traefik basicAuth — the production montage |
| `direct` | `https://kora-direct.ppsk.localhost:8443` | none |

```bash
# Credentials live in the QA Doppler config, the same ones the ingress checks
export KORA_USER=$(doppler secrets get SCHEMA_REGISTRY_USERNAME_DP --plain -p popsink-data-plane -c qa)
export KORA_PASSWORD=$(doppler secrets get SCHEMA_REGISTRY_PASSWORD_DP --plain -p popsink-data-plane -c qa)

just qa-smoke                  # through Traefik + auth
just qa-smoke direct           # same pod, no auth
just qa-stress                 # any scenario takes the same positional target
```

To bypass Traefik entirely — measuring Kora with no proxy in the path at all —
port-forward the Service and use the plain recipes' `KORA_URL`:

```bash
kubectl --context kind-popsink-data-plane-qa port-forward -n kafka svc/kora 8085:8080
```

### Three things to know before reading the numbers

- **The thresholds will fail.** They were calibrated against a native binary on
  localhost. Through Docker, Traefik and TLS they do not hold — re-baseline with
  three `qa-smoke` runs per target rather than treating a breach as a regression.
- **Latency is a Prometheus summary, not a histogram.** Kora builds its recorder
  without buckets, so in Grafana query `http_request_duration_seconds{quantile="0.95"}`
  directly; `histogram_quantile()` does not apply. Labels: `method`, `path`, `status`.
- **Seeding is not idempotent across runs.** `seedSchemas` re-registers the same
  subjects, and `ON CONFLICT DO UPDATE` leaves dead tuples behind, so numbers drift
  on a database a previous run already touched. Recreate the environment
  (`PPSK_TARGET=qa inv stop && PPSK_TARGET=qa inv up`) between runs you intend to
  compare.

## Interpreting results

### Smoke (baseline)

Run 3 times on a clean database. Capture p50/p95/p99 for each operation. These become your baseline for tightening thresholds in other scenarios.

### Stress (pool sweep)

Run with different pool sizes to find the optimal configuration:

```bash
DB_POOL_MAX=10 just stress
DB_POOL_MAX=20 just stress
DB_POOL_MAX=50 just stress
```

Watch for the "knee" — where latency goes non-linear. That's your sustainable capacity.

### Soak (degradation)

Monitor PostgreSQL during the run:

```bash
# In another terminal, periodically:
just monitor
```

Watch for:
- `n_dead_tup` growing on `subjects` (UPSERT creates dead tuples)
- `seq_scan` count increasing on `schema_versions` (missing partial index signal)
- `mean_ms` increasing in `pg_stat_statements` (query degradation)

### Contention (TOCTOU)

The `contention_version_count` custom metric tracks how many versions accumulate. If versions grow unexpectedly, it may indicate the TOCTOU gap (compatibility check runs before the transaction, so two concurrent registrations can both pass the check against stale data).

## Architecture notes

- **Compatibility check runs inside the transaction** — after acquiring the subject row lock, the compat check reads a consistent snapshot. No TOCTOU race between check and insert.
- **UNIQUE on raw_fingerprint** — `schema_contents.raw_fingerprint` has a unique constraint preventing duplicate content rows under concurrent inserts from different subjects.
- **Dead tuple accumulation on subjects** — every registration does `ON CONFLICT DO UPDATE`, creating a dead tuple for existing subjects. Autovacuum handles this well in all tests. Monitor `n_dead_tup` in soak tests.

## Configuration

| Env var | Default | Description |
|---|---|---|
| `KORA_URL` | `http://localhost:8080` | Kora base URL |
| `KORA_USER` | *(unset)* | BasicAuth username. Unset ⇒ no `Authorization` header is sent at all |
| `KORA_PASSWORD` | *(unset)* | BasicAuth password |
| `K6_SOAK_DURATION` | `2h` | Soak test duration |
| `DB_POOL_MAX` | `20` | Kora connection pool size (set on Kora, not k6) |
| `QA_PORT` | `8443` | HTTPS port of the QA ingress (see below) |

## PostgreSQL monitoring

The load test PostgreSQL image has `pg_stat_statements` enabled. Run `just monitor` during tests to see:

- Connection state and lock contention
- Dead tuple accumulation per table
- Top queries by total execution time
- Buffer hit ratio

