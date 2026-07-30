set dotenv-load

image     := env("KORA_IMAGE", "ghcr.io/popsink/kora")
platforms := "linux/amd64,linux/arm64"
pg_ready := "docker compose exec -T postgres pg_isready -U $POSTGRES_USER > /dev/null 2>&1"
db_ready := "docker compose exec -T postgres psql -U $POSTGRES_USER -d $POSTGRES_DB -c 'SELECT 1' > /dev/null 2>&1"

[private]
ensure-pg:
    @{{ pg_ready }} || { docker compose up -d postgres; \
      echo "Waiting for PG..."; until {{ pg_ready }}; do sleep 0.3; done; }
    @until {{ db_ready }}; do sleep 0.3; done

# ---------- Quality ----------

# Check formatting
[group('quality')]
fmt:
    cargo fmt --check

# Run clippy lints
[group('quality')]
lint:
    cargo clippy -- -D clippy::all -D clippy::pedantic

# Auto-fix formatting + clippy suggestions
[group('quality')]
fix:
    cargo fmt
    cargo clippy --fix --allow-dirty -- -D clippy::all -D clippy::pedantic

# ---------- Development ----------

# Run Kora locally with cargo (starts PG automatically)
[group('dev')]
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    just ensure-pg
    trap 'docker compose rm -sf postgres >/dev/null 2>&1 || true' EXIT
    cargo run

# Run the integration suite
[group('dev')]
test:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "${CI:-}" = "true" ]; then
      echo "CI — PG managed by service container"
    elif pg_isready -h localhost -q 2>/dev/null; then
      echo "PG already running — skipping docker compose"
    else
      just ensure-pg
      trap 'docker compose rm -sf postgres >/dev/null 2>&1 || true' EXIT
    fi
    cargo test --test '*' -- --include-ignored

# fmt + lint + test (CI entrypoint)
[group('quality')]
ci: fmt lint test

# ---------- Build & Push ----------

# Build + push the image (static musl, amd64 + arm64)
[group('build')]
build tag="latest":
    docker buildx build --platform {{ platforms }} --provenance=false -f dockerfiles/postgres.Dockerfile -t {{ image }}:{{ tag }} --push .

# ---------- Load testing ----------

loadtest_db  := "postgres://kora:kora@localhost:5433/kora_loadtest"
loadtest_pg  := "docker compose -f loadtest/docker-compose.loadtest.yml"
loadtest_pg_ready := loadtest_pg + " exec -T postgres pg_isready -U kora > /dev/null 2>&1"

[private]
ensure-loadtest-pg:
    @{{ loadtest_pg_ready }} || { {{ loadtest_pg }} up -d; \
      echo "Waiting for load test PG..."; until {{ loadtest_pg_ready }}; do sleep 0.3; done; }
    @{{ loadtest_pg }} exec -T postgres psql -U kora -d kora_loadtest -c "CREATE EXTENSION IF NOT EXISTS pg_stat_statements" > /dev/null 2>&1 || true

# Run a k6 scenario: starts the DB + Kora, tears down after
[private]
loadtest-run scenario *k6args:
    #!/usr/bin/env bash
    set -euo pipefail

    just ensure-loadtest-pg
    cargo build --quiet
    export DATABASE_URL="{{ loadtest_db }}"

    # Start Kora in background.
    DB_POOL_MAX=${DB_POOL_MAX:-20} ./target/debug/kora &
    KORA_PID=$!
    trap 'kill $KORA_PID 2>/dev/null; wait $KORA_PID 2>/dev/null' EXIT

    echo "Waiting for Kora..."
    until curl -sf http://localhost:8080/health > /dev/null 2>&1; do sleep 0.2; done
    echo "Kora ready — running {{ scenario }}"

    k6 run -e KORA_URL=http://localhost:8080 {{ k6args }} loadtest/scenarios/{{ scenario }}

# Quick baseline — 1 VU, 30s
[group('loadtest')]
smoke:
    just loadtest-run smoke.js

# Nominal production load — 5min
[group('loadtest')]
load:
    just loadtest-run load.js

# Find the breaking point — ramp to 300 VUs
[group('loadtest')]
stress:
    just loadtest-run stress.js

# Long-running accumulation — 2h, override K6_SOAK_DURATION
[group('loadtest')]
soak:
    just loadtest-run soak.js --out csv=loadtest/soak-results.csv

# FOR UPDATE lock contention — single subject
[group('loadtest')]
contention:
    just loadtest-run contention.js

# Delete under concurrent writes
[group('loadtest')]
delete-load:
    just loadtest-run delete-under-load.js

# DB monitoring, in another terminal DURING a load test
[group('loadtest')]
monitor:
    {{ loadtest_pg }} exec -T postgres psql -U kora -d kora_loadtest -f /dev/stdin < loadtest/pg-monitor.sql

# Stop load test infrastructure and wipe data
[group('loadtest')]
loadtest-stop:
    {{ loadtest_pg }} down -v

# ---------- Migration ----------

# Audit Karapace and produce karapace-migration/audit.json  (KARAPACE_URL, KARAPACE_USER, KARAPACE_PASSWORD)
[group('migration')]
migrate-audit:
    cd karapace-migration && uv run python audit.py

# Dry-run the migration — show what would be written without touching the DB  (KORA_DB_URL)
[group('migration')]
migrate-dry-run:
    cd karapace-migration && uv run python migrate_direct.py --dry-run

# Run the migration  (KORA_DB_URL)
[group('migration')]
migrate-run:
    cd karapace-migration && uv run python migrate_direct.py

# Verify Kora matches the audit snapshot  (KORA_URL, KORA_USER, KORA_PASSWORD)
[group('migration')]
migrate-verify:
    cd karapace-migration && uv run python verify.py

# ---------- Docker (local) ----------

# Run image locally (needs DATABASE_URL)
[group('docker')]
run db_url:
    docker run --rm --network host --name kora -e "DATABASE_URL={{ db_url }}" {{ image }}:latest

# Stop Kora and compose services
[group('docker')]
stop:
    -docker stop kora
    -docker compose down

# Remove all containers, images, and volumes
[group('docker')]
clean:
    -docker rm -f kora
    -docker rmi {{ image }}:latest
    -docker compose down -v
