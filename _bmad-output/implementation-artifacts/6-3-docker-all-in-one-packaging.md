# Story 6.3: Docker All-in-One Packaging

Status: done

**Depends on:** Stories 6.1 and 6.2 complete (mode control + metrics)

## Story

As an **operator**,
I want a Docker image that runs Kora with embedded PostgreSQL,
so that I can deploy with zero external dependencies.

## Acceptance Criteria

### AC1: All-in-One Default Mode

**Given** the Docker image is built
**When** I run `docker run -p 8080:8080 kora`
**Then** embedded PostgreSQL starts automatically
**And** Kora starts and connects to the embedded PG
**And** `GET /health` returns HTTP 200

### AC2: External Database Mode

**Given** `DATABASE_URL` is set
**When** I run `docker run -e DATABASE_URL=postgres://... kora`
**Then** embedded PostgreSQL does NOT start
**And** Kora connects to the external PG

### AC3: Graceful Shutdown

**Given** a running container
**When** I send `docker stop`
**Then** both Kora and PG shut down gracefully

### AC4: Multi-Architecture Support

**Given** a build with `--platform linux/amd64,linux/arm64`
**When** the image is pushed to a registry
**Then** both amd64 and arm64 platforms are available

## Tasks / Subtasks

- [x] Task 1: Optimize Cargo release profile (AC: 4)
  - [x] 1.1: Add `[profile.release]` to `Cargo.toml`: `lto = true`, `codegen-units = 1`, `opt-level = "s"`, `strip = true`
  - [x] 1.2: Release binary: 6.9MB (down from ~15MB)

- [x] Task 2: Rewrite Dockerfile — Alpine + musl + xx cross-compile (AC: 1, 2, 4)
  - [x] 2.1: Builder stage: `tonistiigi/xx` + `rust:1.94-alpine`, `xx-cargo build --release`, `xx-verify --static`
  - [x] 2.2: Runtime stage: `alpine:3.21` + `postgresql17` + `tini` + `su-exec`
  - [x] 2.3: Default ENV: `HOST=0.0.0.0`, `PORT=8080`
  - [x] 2.4: Expose 8080
  - [x] 2.5: Optional persistence via `-v kora-data:/var/lib/postgresql/data`
  - [x] 2.6: OCI labels (`image.source`, `image.description`, `image.licenses`)
  - [x] 2.7: `HEALTHCHECK` via `wget -qO- http://localhost:8080/health`

- [x] Task 3: Create entrypoint script — `docker/entrypoint.sh` (AC: 1, 2, 3)
  - [x] 3.1: ~38 line script: initdb, pg_ctl start, pg_isready, createuser/createdb, trap, kora background + wait
  - [x] 3.2: chmod +x in git
  - [x] 3.3: Signal forwarding: trap relays TERM/INT/QUIT to kora PID, waits, then cleanup PG
  - [x] 3.4: PG logs to `/var/lib/postgresql/pg.log` (not /dev/null) for debuggability
  - [x] 3.5: Explicit `pg_isready -t 5` check after `pg_ctl start`
  - [x] 3.6: Non-superuser `createuser kora` (no `-s` flag)

- [x] Task 4: Handle SIGTERM in Kora (AC: 3)
  - [x] 4.1: `shutdown_signal()` handles SIGINT + SIGTERM via `tokio::select!` with `#[cfg(unix)]`
  - [x] 4.2: `sigterm.recv()` → `None` guard with `tracing::warn`
  - [x] 4.3: Signal flow: docker stop → SIGTERM → tini → shell trap → kill kora → wait → cleanup PG → exit

- [x] Task 5: Create .dockerignore (AC: 1)
  - [x] 5.1: Excludes target/, .git/, _bmad*/, tests/, .env, docs/, *.md

- [x] Task 6: Update docker-compose.yml (AC: 1, 2)
  - [x] 6.1: Removed `api` service (replaced by `just dev` for local dev, Docker images for prod)
  - [x] 6.2: Kept `postgres` service for `just dev`/`just test` workflow

- [x] Task 7: Update justfile with build/push targets (AC: 4)
  - [x] 7.1: Grouped recipes: `[dev]`, `[build]`, `[docker]`
  - [x] 7.2: `just build` — buildx multi-arch slim image → ghcr.io
  - [x] 7.3: `just build-embedded` — buildx multi-arch all-in-one image → ghcr.io
  - [x] 7.4: `just release` — builds + pushes both images (slim last → featured on ghcr.io)
  - [x] 7.5: Versioning via tag parameter: `just release v0.4.0`

- [x] Task 8: Container registry setup (AC: 4)
  - [x] 8.1: Registry: `ghcr.io/Popsink/kora`
  - [x] 8.2: OCI labels: `image.source`, `image.description`, `image.licenses` → auto-links to GitHub repo
  - [x] 8.3: `--provenance=false` to avoid "unknown" platform entries
  - [x] 8.4: buildx builder (`kora-builder`, driver `docker-container`) + QEMU for arm64 emulation

- [x] Task 9: End-to-end smoke tests from ghcr.io (AC: 1, 2, 3, 4)
  - [x] 9.1: `docker pull ghcr.io/Popsink/kora:latest-embedded` — success
  - [x] 9.2: `docker pull ghcr.io/Popsink/kora:latest` — success
  - [x] 9.3: Image sizes: slim ~24MB, embedded ~73MB
  - [x] 9.4: Embedded: `docker run -p 8080:8080 ghcr.io/Popsink/kora:latest-embedded` → PG auto-init, health UP, register Avro/JSON/Protobuf, schema evolution, compatibility test, soft delete + ID still resolvable, metrics
  - [x] 9.5: Slim + external PG: `docker run -e DATABASE_URL=... ghcr.io/Popsink/kora:latest` → health UP, register + query, PG tables verified via psql (6 tables, rows correct)
  - [x] 9.6: Graceful shutdown: `docker stop` → exit code 0 on both images
  - [x] 9.7: Data persistence: `-v kora-data:/var/lib/postgresql/data` → schema survives container restart
  - [x] 9.8: Multi-arch: both tags on ghcr.io with amd64 + arm64 manifests, no "unknown" entries

- [x] Task 10: Adversarial code review (5 passes) (AC: all)
  - [x] 10.1: Pass 1-2: fixed signal forwarding (exec→background+wait), pg_ctl error handling, pg_isready, superuser removal, PG logs to file, HEALTHCHECK, rust:1.94 pinned, opt-level z→s, image overridable, curl→wget, db_url quoted
  - [x] 10.2: Pass 3-4: fixed trap exit code (143→propagate real), kill -TERM relay to kora PID
  - [x] 10.3: Pass 5: 3/3 reviewers LGTM, edge case hunter returns []

- [x] Task 11: README rewrite (AC: all)
  - [x] 11.1: Quick start (embedded, external, verify), images table, config table, full API reference, deployment examples (K8s, Compose, standalone), development recipes, architecture
  - [x] 11.2: Verified all README commands work as documented (docker pull → run → health → register → persist)

## Dev Notes

### Architecture: Alpine + musl + tini (Tansu-inspired)

Inspired by Tansu's Dockerfile. Key design decisions:

| Decision | Choice | Why |
|---|---|---|
| Base image | `alpine:3.21` | ~7MB, PG available via apk |
| Build | `xx-cargo` musl static | Multi-arch in one build, ~6MB binary |
| PG package | `postgresql17` full apk | Simple, maintained by Alpine. ~30MB. |
| Process supervisor | `tini` + shell script | 30KB PID 1. No s6-overlay (overkill for 2 processes). |
| Signal handling | `tini` → entrypoint trap → kora SIGTERM | Clean shutdown chain |

**Actual image sizes:** slim ~24MB, embedded ~73MB

### Cross-Compilation with `tonistiigi/xx`

`xx` is a Docker helper for cross-compilation. It provides:
- `xx-cargo` — wraps cargo with correct target triple and linker
- `xx-apk` — installs target-arch libraries
- `xx-verify` — verifies the binary is correctly linked

The `$BUILDPLATFORM` / `$TARGETPLATFORM` args are injected by Docker BuildKit. A single `docker buildx build --platform linux/amd64,linux/arm64` produces both architectures.

**Why musl works for Kora**: all dependencies are pure Rust or use `rustls` (not OpenSSL). No C library dependency beyond musl itself. `sqlx` uses `tls-rustls-ring-webpki` — pure Rust TLS.

### Container Registry (ghcr.io)

Two images published to `ghcr.io/Popsink/kora`:

| Tag | Content | Use case |
|---|---|---|
| `latest` (or `vX.Y.Z`) | Slim — no embedded PG | K8s, prod with external PG |
| `latest-embedded` (or `vX.Y.Z-embedded`) | All-in-one — embedded PG17 | Standalone prod, edge, demos |

Both are multi-arch manifests (amd64 + arm64). `docker pull` resolves the correct platform automatically.

**OCI labels** on the image (`org.opencontainers.image.source`, `.description`, `.licenses`) auto-link the package to the GitHub repo and populate the description.

**Buildx setup** (one-time):
```bash
docker run --rm --privileged multiarch/qemu-user-static --reset -p yes
docker buildx create --name kora-builder --driver docker-container --use --bootstrap
```

**`--provenance=false`** is set on all buildx commands to prevent attestation manifests from appearing as "unknown" platform entries on ghcr.io.

### Entrypoint Script Design

Two paths: embedded PG (kora in background + wait) or external PG (exec kora directly).

```sh
#!/bin/sh
set -e

if [ -z "$DATABASE_URL" ]; then
    if ! command -v initdb > /dev/null 2>&1; then
        echo "ERROR: DATABASE_URL is required (image built without embedded PostgreSQL)" >&2
        exit 1
    fi
    PGDATA="/var/lib/postgresql/data"
    if [ ! -f "$PGDATA/PG_VERSION" ]; then
        su-exec postgres initdb -D "$PGDATA" --auth=trust
    fi
    su-exec postgres pg_ctl start -D "$PGDATA" -l /var/lib/postgresql/pg.log -w -t 30 \
        || { echo "ERROR: PostgreSQL failed to start"; cat /var/lib/postgresql/pg.log >&2; exit 1; }
    su-exec postgres pg_isready -t 5 || { echo "ERROR: PostgreSQL not accepting connections" >&2; exit 1; }
    su-exec postgres createuser kora 2>/dev/null || true
    su-exec postgres createdb -O kora kora 2>/dev/null || true
    export DATABASE_URL="postgres://kora@localhost:5432/kora"
    cleanup() { su-exec postgres pg_ctl stop -D "$PGDATA" -m fast -w 2>/dev/null || true; }
    trap 'kill -TERM $KORA_PID 2>/dev/null; wait $KORA_PID; cleanup; exit' TERM INT QUIT
    /usr/local/bin/kora "$@" &
    KORA_PID=$!
    wait $KORA_PID
    EXIT_CODE=$?
    cleanup
    exit $EXIT_CODE
fi
exec /usr/local/bin/kora "$@"
```

Key points:
- `su-exec` (Alpine's `gosu` equivalent) runs PG commands as `postgres` user
- `initdb` only runs on first start (checks `PG_VERSION` file)
- `pg_ctl start -w -t 30` waits for PG, logs to file (not /dev/null) for debuggability
- `pg_isready -t 5` explicit readiness check after startup
- `createuser kora` — non-superuser (no `-s` flag)
- Embedded path: kora runs in background (`&`), shell `wait`s — trap stays active
- Trap relays `kill -TERM` to kora, waits for it, then stops PG
- External path: `exec kora` replaces the shell — tini forwards signals directly

### Signal Flow on `docker stop`

**Embedded PG path:**
```
docker stop → SIGTERM → tini (PID 1) → shell (trap fires)
  trap: kill -TERM $KORA_PID → kora graceful shutdown → wait returns
  trap: cleanup → pg_ctl stop → container exits
```

**External PG path:**
```
docker stop → SIGTERM → tini (PID 1) → kora (exec'd, direct child)
  kora: tokio graceful shutdown → container exits
```

`tini` ensures signals reach the child process and reaps zombies.

### Kora SIGTERM Handler

```rust
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ).expect("failed to install SIGTERM handler");
        tokio::select! {
            result = ctrl_c => { result.expect("failed to listen for CTRL+C"); },
            recv = sigterm.recv() => {
                if recv.is_none() {
                    tracing::warn!("SIGTERM stream closed unexpectedly");
                }
            },
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await.expect("failed to install CTRL+C handler");
    tracing::info!("shutdown signal received");
}
```

### Release Profile

```toml
[profile.release]
lto = true
codegen-units = 1
opt-level = "s"
strip = true
```

- `lto = true` — link-time optimization across all crates
- `codegen-units = 1` — better optimization (slower build, smaller binary)
- `opt-level = "s"` — optimize for size with acceptable runtime performance
- `strip = true` — remove debug symbols

### PostgreSQL Data Persistence

By default, data is ephemeral (lost on `docker rm`). For persistence:
```
docker run -v kora-data:/var/lib/postgresql/data -p 8080:8080 kora
```

Document this in README, not enforced by the image.

### Justfile Structure

```
[build]  build, build-embedded, release   → buildx multi-arch + push to ghcr.io
[dev]    dev, test, lint                   → local development (cargo + PG compose)
[docker] run, run-embedded, stop, clean    → run images locally
```

Versioning via tag parameter: `just release v0.4.0`
Override registry: `KORA_IMAGE=my-registry.io/kora just release`

### Dev Workflow Unchanged

`just dev` runs cargo + PG via Docker Compose. `just test` starts PG automatically. The all-in-one mode is for the standalone Docker image.

### Files Created

| File | Purpose |
|---|---|
| `docker/entrypoint.sh` | Entrypoint: PG auto-detect, init, start, trap, background kora + wait |
| `.dockerignore` | Build context exclusions |

### Files Modified

| File | Change |
|---|---|
| `Cargo.toml` | `[profile.release]` with LTO + strip + opt-level "s" |
| `Dockerfile` | xx cross-compile + Alpine + PG17 + tini + OCI labels + HEALTHCHECK |
| `docker-compose.yml` | Removed `api` service (replaced by Docker images) |
| `justfile` | Grouped recipes `[dev]`/`[build]`/`[docker]`, buildx multi-arch, ghcr.io push |
| `src/main.rs` | SIGTERM handling in `shutdown_signal()` with `#[cfg(unix)]` |
| `README.md` | Complete rewrite: quick start, images, API, deployment, development |

### Previous Story Intelligence

From story 6.2:
- `main.rs` installs Prometheus recorder before pool creation — SIGTERM fix goes in `shutdown_signal()` at the bottom
- `api::router()` takes `(pool, metrics_handle, max_body_size)` — no changes needed
- `/health` endpoint is the AC1 smoke test target
- `/metrics` endpoint can verify the full stack is working

### References

- [Source: github.com/tansu-io/tansu/Dockerfile] — xx cross-compile + FROM scratch pattern (inspiration)
- [Source: _bmad-output/planning-artifacts/epics.md#Story 6.3] — Acceptance criteria
- [Source: _bmad-output/planning-artifacts/prd.md#FR42] — Docker embedded PG requirement
- [Source: _bmad-output/planning-artifacts/architecture.md#Binary Distribution] — Multi-stage build
- [Source: Dockerfile] — Current simple multi-stage build
- [Source: docker-compose.yml] — Current dev setup (separate PG)
- [Source: src/main.rs] — shutdown_signal() needs SIGTERM
- [Source: _bmad-output/implementation-artifacts/6-2-prometheus-metrics.md] — Previous story

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (1M context)

### Debug Log References

### Completion Notes List

- **Task 1**: `[profile.release]` with LTO, codegen-units=1, opt-level=s, strip=true.
- **Task 2**: Dockerfile rewrite. Builder: `tonistiigi/xx` + `rust:1.94-alpine`. Runtime: `alpine:3.21` + `postgresql17` + `tini` + `su-exec`. OCI labels + HEALTHCHECK via wget. Slim ~24MB, embedded ~73MB.
- **Task 3**: `docker/entrypoint.sh` (~38 lines). Embedded: initdb → pg_ctl start (logs to file) → pg_isready → createuser (non-superuser) → kora background → wait. External: exec kora. Signal trap relays TERM to kora PID.
- **Task 4**: `shutdown_signal()` handles SIGINT + SIGTERM via `tokio::select!` with `#[cfg(unix)]`. `recv().is_none()` guarded with warn.
- **Task 5**: `.dockerignore` excluding target/, .git/, _bmad*/, tests/, .env, docs/, *.md.
- **Task 6**: Removed `api` service from docker-compose.yml. Kept `postgres` for dev workflow.
- **Task 7**: Justfile: grouped `[dev]`/`[build]`/`[docker]`, buildx multi-arch, ghcr.io push, `--provenance=false`, overridable image via `KORA_IMAGE` env.
- **Task 8**: ghcr.io registry: OCI labels auto-link to repo, QEMU + buildx builder for arm64.
- **Task 9**: E2E tests from ghcr.io: both images pulled, embedded (all 3 formats, evolution, compat, soft delete, persistence, metrics), slim + external PG (register, query, psql table inspection), graceful shutdown both.
- **Task 10**: 5 adversarial review passes (blind + cynical + edge case hunter). All converge to LGTM on pass 5.
- **Task 11**: README rewrite. All documented commands verified by running them.

### Change Log

- 2026-04-15: Implemented Docker all-in-one packaging. Alpine + musl static + tini + PG17 embedded. Multi-arch (amd64/arm64) on ghcr.io. SIGTERM handling. HEALTHCHECK. 5-pass adversarial review. Complete README rewrite with verified examples.

### File List

- Cargo.toml (modified) — `[profile.release]` with LTO + strip + opt-level "s"
- Dockerfile (rewritten) — xx cross-compile + Alpine runtime + PG17 + tini + OCI labels + HEALTHCHECK
- docker/entrypoint.sh (new) — PG auto-detect, init, start, background kora + wait, signal relay
- .dockerignore (new) — build context exclusions
- docker-compose.yml (modified) — removed `api` service
- justfile (rewritten) — grouped recipes, buildx multi-arch, ghcr.io push
- src/main.rs (modified) — SIGTERM handling in `shutdown_signal()`
- README.md (rewritten) — quick start, images, API, deployment, development
