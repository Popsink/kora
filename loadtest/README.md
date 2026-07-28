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
| `K6_SOAK_DURATION` | `2h` | Soak test duration |
| `DB_POOL_MAX` | `20` | Kora connection pool size (set on Kora, not k6) |

## PostgreSQL monitoring

The load test PostgreSQL image has `pg_stat_statements` enabled. Run `just monitor` during tests to see:

- Connection state and lock contention
- Dead tuple accumulation per table
- Top queries by total execution time
- Buffer hit ratio

