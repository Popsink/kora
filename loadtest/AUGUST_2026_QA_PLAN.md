# Kora — August 2026 QA Plan (scale & load)

> Part of the Popsink August 2026 non-functional QA effort. Sister plans live in
> `data-plane/docs/qa/`, `kotatsu/e2e/`, and `popsink-partner-portal/e2e_tests/`.

## Objective

Prove Kora holds its latency and correctness at **production-plus scale (30 000 schemas)**
and under sustained concurrent load, and publish per-endpoint latency baselines
(with a focus on **p99 tail latency**) that become the reference for regression.

## Guardrails (non-negotiable)

- 🚫 **No client / real data.** All fixtures are synthetic (`avro-perf-{i}`,
  `json-perf-{i}`, `proto-perf-{i}`). This repo is public.
- Tests run against a **dedicated, disposable environment** — never prod, never a shared stack.

## Tooling & environment

- **k6** — already the standard here (`loadtest/`). Reuse the existing scenario
  taxonomy and `helpers.js` (tagged HTTP helpers, `SharedArray` fixtures).
- **Dedicated env** stood up with **Balkis (SRE)**; k6 metrics exported to
  **Prometheus → Grafana** for shared, historical dashboards.
- Local quick look: k6 built-in web dashboard
  (`K6_WEB_DASHBOARD=true` / `K6_WEB_DASHBOARD_EXPORT=report.html`).
- PostgreSQL monitored via `pg_stat_statements` (`just pg-monitor`).

## Test topology (phased)

- **Phase 1 — Kora direct (no Traefik, no auth).** Hit Kora's port directly to get
  its **raw** per-endpoint baseline, with no reverse-proxy/auth noise. The existing 6
  scenarios already target Kora directly and carry no auth token, so no rework.
- **Phase 2 — prod-like (Traefik + auth in front).** Add the reverse proxy and the
  production auth to measure the **realistic end-to-end** path; comparing Phase 1 vs
  Phase 2 quantifies the proxy/auth overhead. k6 scripts gain the auth token in this
  phase. The dedicated env should expose **both** paths so no second infra request is needed.

## Starting point (what already exists)

- Scenarios: `smoke` / `load` / `stress` / `soak` / `contention` / `delete-load`
  driven by the `justfile`.
- Base corpus in `helpers.js`: 500 Avro + 200 JSON + 100 Protobuf = **800 subjects**.
- Thresholds are currently **p95**-based.

## Workstreams

### WS-K1 — Establish the baseline (do this first)
- Run `just smoke` **×3 on a clean DB**; capture p50 / p95 / **p99** per endpoint.
- Add **p99** to `summaryTrendStats` and to the thresholds (today they are p95 only)
  so the "last 1%" is a first-class pass/fail signal.
- Reference baseline captured 2026-08-04 (1 VU, image `ghcr.io/popsink/kora:latest`):
  reads p95 < 5 ms (`list_subjects` ~1.9 ms, `get_by_version` ~3.5 ms,
  `get_by_id` ~4.9 ms), `register` (write) p95 ~60 ms / max ~235 ms, 0 errors.

### WS-K2 — Scale to 30 000 schemas
- Extend the corpus / add a `scale-30k` seed (30 000 subjects, keeping the
  60/25/15 Avro/JSON/Proto mix) — generate deterministically, do **not** load 30k
  into a single `SharedArray` (memory); seed via `setup()` in batches.
- Measure at scale: `GET /subjects` (list, pagination), `GET /schemas/ids/{id}`,
  `GET /subjects/{s}/versions/{v}`, compatibility checks.
- Watch for the read-path degradation the loadtest README already flags:
  `seq_scan` growth on `schema_versions` (missing/partial index signal),
  `n_dead_tup` on `subjects` (UPSERT churn).

### WS-K3 — Sustained load & breaking point
- `just load` (nominal) then `just stress` (ramp 10 → 300 VUs) to find the **knee**.
- Sweep `DB_POOL_MAX` (10 / 20 / 50) to locate the sustainable pool size.
- `just soak` (2h) at 30k-schema scale to catch bloat / query drift; monitor PG throughout.
- `just contention` to re-confirm the TOCTOU / `FOR UPDATE` behaviour under scale.

## Metrics & acceptance

- Report per run: **p50 / p95 / p99** and max per endpoint, throughput (req/s),
  error rate, plus the PG signals above.
- **p99 thresholds are derived from the WS-K1 baseline**, not guessed. Provisional
  gates to refine: reads `p(99) < 50 ms`, writes `p(99) < 250 ms`, `http_req_failed rate == 0`.
- A run **fails** if a p99 gate is exceeded or any request errors.

## Deliverables

- `scale-30k` seed + scenario, and p99-aware thresholds committed under `loadtest/`.
- A per-run perf report (HTML export and/or Grafana snapshot) with the knee and pool-sweep results.
- Issues for any regression: **backend → Romain**.

## Risks

- August holidays (FR) → lock Balkis's slot for the dedicated env early (critical path).
- 30k seed time can be long — seed once, snapshot the DB volume for reuse.
- Thresholds are meaningless without WS-K1 → baseline gates everything else.
