# Kora — Load & Scale Performance Report

**Date:** 2026-08-11 · **Tester:** QA (Haydir) · **Component:** Kora (schema registry)
**Part of:** August 2026 non-functional QA effort (`loadtest/AUGUST_2026_QA_PLAN.md`)

> **Data policy:** all data synthetic (`avro-perf-*`, `stress*-*`). No client data.

## TL;DR

- Kora is **rock-solid on reliability**: **0 errors across ~1.7M requests**, from 1 to 2500 concurrent users. It never crashes — under overload it degrades **gracefully into latency**, not failures.
- We found a **clean saturation point**: throughput plateaus at **~1200 req/s** for this configuration. Beyond ~1000 concurrent users, extra load produces **no additional throughput** — only queuing (latency wall).
- **Writes degrade first** (`register`): they hit multi-second p99 well before reads.
- The ceiling is bounded by the tested **config** (1 replica, DB pool = 20), not by a Kora defect.

## Environment & method

| Item | Value |
|---|---|
| Kora | deployed via Helm chart in the local **QA kind cluster** (`PPSK_TARGET=qa`) |
| Topology | **prod-like**: client → Traefik (TLS) → basicAuth middleware → Kora → PostgreSQL |
| Replicas / DB pool | **1 replica**, `DB_POOL_MAX=20` |
| Tool | **k6** (run natively on the host), scenarios in `loadtest/scenarios/` |
| Reporting | k6 → Prometheus (remote write) → Grafana (dashboard 19665); focus **p99** |
| Route tested | `kora.ppsk.localhost:8443` (auth). Direct/no-auth route available for comparison. |

**Validity note:** this is a **single-replica, local** cluster with the **load generator co-located** on the same laptop. Absolute numbers are **not** production figures — they are valid for **relative comparison** and for locating **this config's** saturation point. For all runs below **swap stayed at 0**, so the numbers reflect Kora/the cluster, not laptop memory pressure.

## Results — capacity curve

| Scenario | VUs | Throughput | Global p95 | Global p99 | Errors |
|---|---|---|---|---|---|
| `smoke` | 1 | ~15 req/s | ~3 ms | ~5 ms | 0 |
| `load` | ~50–74 | 167 req/s | 82 ms | 110 ms | 0 |
| `stress` | 300 | 611 req/s | 384 ms | 482 ms | 0 |
| `stress-hard` | 1000 | **1207 req/s** | 1.47 s | 1.91 s | 0 |
| `stress-break` | 2500 | **1216 req/s** ⬅ plateau | 4.23 s | 5.0 s | 0 |

**The knee:** throughput climbs 15 → 167 → 611 → 1207 req/s, then **flatlines**: 1000→2500 VUs adds only **+9 req/s (+0.7%)** for **+150%** load, while global p99 balloons **1.9 s → 5.0 s**. That flat-throughput / rising-latency signature **is** the saturation point.

## Per-endpoint p99 (prod-like auth route)

| Endpoint | p99 @ 300 VUs | p99 @ 1000 VUs |
|---|---|---|
| `GET /subjects/{s}/versions/{v}` (get_by_version) | — | 93 ms |
| `GET /schemas/ids/{id}` (get_by_id) | 190 ms | 667 ms |
| `GET /subjects` (list_subjects) | — | 184 ms |
| `POST /compatibility/...` (compat) | — | 191 ms |
| `POST /subjects/{s}` (check_schema) | — | 205 ms |
| `POST /subjects/{s}/versions` (**register**, write) | 587 ms | **2.1 s** |

Reads stay in the hundreds-of-ms range; **writes are the first to become painful** (register p99 = 2.1 s at 1000 VUs) — consistent with DB connection-pool contention on writes.

## Reliability

- **0 errors / 0 interrupted iterations** on every run (≈1.7M requests total).
- Degradation mode = **latency**, never failure. No 5xx, no dropped connections, no crash — even at 2500 concurrent users.

## Bottleneck analysis

The saturation at ~1200 req/s + writes-degrade-first strongly points to **the single replica and the DB connection pool (`DB_POOL_MAX=20`)** as the limiter, not Kora's request handling itself.

## Recommendations

1. **To raise the ceiling:** run **more Kora replicas** and/or **increase `DB_POOL_MAX`**. A pool sweep (10 / 20 / 50) on the deployed instance would confirm the pool as the limiter.
2. **Set explicit p99 SLOs** (e.g., reads p99 < 100 ms, writes p99 < 500 ms) and re-baseline the scenario thresholds against this env (the shipped thresholds are calibrated on bare-metal localhost and fail here by design).
3. **For the ABSOLUTE breaking point:** use a **dimensioned environment** with the **load generator on a separate machine** — the local laptop caps VU count (Docker VM ≈ 9.6 GB of 24 GB) and co-locating k6 with the cluster limits how hard we can push cleanly.

## Artifacts

- Scenarios: `loadtest/scenarios/{smoke,load,stress,stress-hard,stress-break}.js`
- k6 QA recipes: `just qa-smoke|qa-load|qa-stress [direct]` (Docker) / native k6 for Grafana output
- Grafana: dashboard **19665** ("k6 Prometheus")

## Next steps

- Compare **auth vs direct** route under load → quantify the basic-auth cost (at 1 VU it was negligible).
- Extend to **30k-schema scale** (`scale-30k.js`) and to **Kotatsu**.
- Re-run on the dimensioned env when available (with Balkis / the senior SRE).
