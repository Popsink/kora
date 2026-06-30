# Load Tests

k6 load tests for Kora schema registry.

## Prerequisites

- [k6](https://grafana.com/docs/k6/latest/set-up/install-k6/) installed (`brew install k6`)
- Docker
- Rust toolchain

## Quick start

```bash
just smoke          # Postgres (default)
just smoke oracle   # Oracle (gvenzl/oracle-free; boots a fresh DB each run, ~2-4 min)
```

Every recipe takes an optional backend argument — `postgres` (the default) or `oracle`. `just` arguments are **positional**, so it's `just smoke oracle`, not `backend=oracle`. Either way the recipe automatically:
1. Starts a dedicated database — PostgreSQL (port 5433, `pg_stat_statements`) or Oracle Free (port 1521)
2. Builds Kora (with `--features oracle` for Oracle) and starts it against that DB
3. Runs the k6 scenario (scenarios are backend-agnostic — they only hit Kora's HTTP API)
4. Kills Kora when done

Override pool size: `DB_POOL_MAX=50 just stress` (or `DB_POOL_MAX=50 just stress oracle`).

> Only one backend runs at a time; the Oracle service shares port 1521 with the dev/test Oracle, so stop those first. `just loadtest-stop` tears down both and wipes the volumes.

## Scenarios

| Recipe | Scenario | VUs | Duration | Purpose |
|---|---|---|---|---|
| `just smoke` | smoke.js | 1 | 30s | Baseline — full user journey, establish latency floor |
| `just load` | load.js | 50+ readers, 20 writes/s, 5 compat, 3 check | 5 min | Nominal production load |
| `just stress` | stress.js | 10 → 300 (ramp) | 6 min | Find the breaking point, pool saturation |
| `just soak` | soak.js | 30 | 2h | Query degradation, dead tuples, table bloat |
| `just contention` | contention.js | 10 → 50 (ramp) | 5 min | FOR UPDATE lock, MAX(version)+1, TOCTOU |
| `just delete-load` | delete-under-load.js | 10 writers + 5 deleters + 5 readers | 3 min | Delete race conditions, reference protection |

Append `oracle` to any recipe (e.g. `just stress oracle`, `just soak oracle`) to run the same HTTP load against Oracle instead of Postgres — only the backend Kora talks to changes.

## Oracle backend

Kora supports Oracle in production, so the same scenarios run against it: `just <scenario> oracle` (e.g. `just stress oracle`) spins up Oracle Free, builds Kora with `--features oracle`, and points it at `FREEPDB1`.

> **⚠️ Driver limit (mitigated).** The pure-Rust `oracle-rs` 0.1 driver **leaks one server cursor per statement** for the connection's lifetime (it never closes them), so an un-managed connection would be dropped by Oracle once it reaches `open_cursors` (~300). Kora's Oracle pool works around this by **retiring a connection before it crosses a statement threshold** (`ORACLE_MAX_QUERIES_PER_CONN`, default 200), tearing down the session to free its cursors — the per-statement analogue of HikariCP's `maxLifetime`. Under sustained load (`stress`, `soak`) connections are recycled transparently, so reads should **not** show a baseline error rate. Watch the leak/recycle live with `just monitor oracle`: open cursors per session should climb toward the threshold and then reset as connections retire.

> **Sizing.** Retirement is checked at **checkout**, so a single request can still burst statements on a borrowed connection (a large paginated listing, or a transitive-compatibility scan over a long version history). The invariant is `ORACLE_MAX_QUERIES_PER_CONN + worst-case single-request burst < open_cursors`. It's a balance: too high overflows `open_cursors`; too **low** churns reconnections (which under heavy load can overwhelm Oracle and surface as `connection refused`). The right operating point is a **generous `open_cursors`** with a moderate threshold — exactly how a production DB is configured. The load-test harness therefore raises the Oracle instance to `open_cursors = 1000` on boot (a stock gvenzl image defaults to 300), giving the default threshold (200) ~800 cursors of headroom. In production, set `open_cursors` similarly and tune the threshold to taste.

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

## Oracle monitoring

Run `just monitor oracle` during an Oracle test (connects as `SYSTEM`, since the `V$` views aren't granted to `kora`) to see:

- The `open_cursors` ceiling and **open cursors per `kora` session** — the key metric: watch it climb toward the ceiling (the driver cursor leak in action)
- Active `kora` sessions and their wait events
- Top SQL by elapsed time
- Blocking sessions (lock contention)
