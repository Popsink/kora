<div align="center">

# Kora

**A Confluent-compatible Schema Registry, built in Rust.**

PostgreSQL or Oracle storage · Single binary · Sub-millisecond lookups · Zero JVM overhead

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![ghcr.io](https://img.shields.io/badge/ghcr.io-popsink%2Fkora-blue?logo=docker)](https://github.com/Popsink/kora/pkgs/container/kora)

</div>

## Why Kora?

| | Confluent | Karapace | Kora |
|---|---|---|---|
| **Storage** | Kafka topic | Kafka topic | PostgreSQL / Oracle |
| **Runtime** | JVM | Python | Native (Rust) |
| **Kafka dependency** | Required | Required | None |
| **API compatibility** | Reference | Partial | 100% wire-compatible |

Existing serializers, connectors, and CLI tools work without modification.

## Quick Start

```yaml
# docker-compose.yml
services:
  postgres:
    image: postgres:17-alpine
    environment: { POSTGRES_DB: kora, POSTGRES_USER: kora, POSTGRES_PASSWORD: kora }
  kora:
    image: ghcr.io/popsink/kora:latest
    depends_on: [postgres]
    environment: { DATABASE_URL: "postgres://kora:kora@postgres:5432/kora" }
    ports: ["8080:8080"]
```

```bash
docker compose up -d
curl http://localhost:8080/health
# {"status":"UP"}
```

## Install

### Helm (recommended)

```bash
helm install kora oci://ghcr.io/popsink/kora/charts/kora \
  --set database.host=my-postgres.example.com \
  --set database.password=secret
```

See [`chart/README.md`](chart/README.md) for all options.

### Docker

```bash
docker run -p 8080:8080 -e DATABASE_URL="postgres://user:pass@host:5432/kora" ghcr.io/popsink/kora:latest
```

## Configuration

| Variable | Default | Description |
|---|---|---|
| `DB_BACKEND` | `postgres` | Backing store engine: `postgres` or `oracle`. Inferred from the `DATABASE_URL` scheme when unset (`oracle://` → Oracle) |
| `DATABASE_URL` | *(required)* | Connection string. PostgreSQL: `postgres://…`. Oracle: `oracle://user:pass@host:port/service`. If empty, composed from the `DB_*` components below |
| `DB_HOST` / `DB_PORT` / `DB_USER` / `DB_PASSWORD` / `DB_NAME` | — | Connection components used when `DATABASE_URL` is empty. For Oracle, `DB_NAME` is the **service name** and `DB_PORT` defaults to `1521` |
| `HOST` | `0.0.0.0` | Server bind address |
| `PORT` | `8080` | Server listen port |
| `MAX_BODY_SIZE` | `16777216` | Maximum request body size in bytes |
| `DB_POOL_MAX` | `20` | Maximum database connections |
| `RUST_LOG` | `info` | Log level (`error`, `warn`, `info`, `debug`, `trace`) |
| `DEFAULT_COMPATIBILITY` | *(unset)* | Default global compatibility level, re-applied to the global config on **every startup** — overwriting any runtime `PUT`/`DELETE /config` change to the global level. One of `BACKWARD`, `BACKWARD_TRANSITIVE`, `FORWARD`, `FORWARD_TRANSITIVE`, `FULL`, `FULL_TRANSITIVE`, `NONE`. Leave unset (default) to let the runtime API own the level (`BACKWARD` on a fresh install) |

### Oracle backend

PostgreSQL is the default and the standard build. Oracle support is **additive and
opt-in** — existing PostgreSQL deployments are unaffected, and the wire API stays
100% Confluent-compatible on either engine. Kora connects to an **external** Oracle
database (it never hosts one), exactly as it does for PostgreSQL.

Oracle uses the pure-Rust [`oracle-rs`](https://crates.io/crates/oracle-rs) driver
(Oracle TNS protocol over TCP), so there is **no Oracle Instant Client and no native
dependency**. It is gated behind the `oracle` cargo feature only to keep the default
build minimal; an Oracle-enabled image is the **same single static binary**, just
built with the feature:

```bash
# Oracle-enabled image — still a static musl binary, no Instant Client.
docker build --build-arg CARGO_FEATURES=oracle -t kora:oracle .

docker run -p 8080:8080 \
  -e DB_BACKEND=oracle \
  -e DATABASE_URL="oracle://kora:secret@oracle-host:1521/FREEPDB1" \
  kora:oracle
```

Via Helm, set `database.backend=oracle` (and point `image` at an Oracle-enabled
image). Supported: **Oracle 19c+** (identity columns, 128-char identifiers);
exercised in CI against Oracle Free. Schema migrations run automatically on startup.

## API

Kora implements the full [Confluent Schema Registry REST API](https://docs.confluent.io/platform/current/schema-registry/develop/api.html) — Avro, JSON Schema, and Protobuf with all 7 compatibility modes.

## Development

Requires [just](https://github.com/casey/just), [Rust](https://rustup.rs/), and [Docker](https://docs.docker.com/get-docker/).

```bash
just dev          # Run locally (starts PG via Docker Compose)
just test         # Run all tests (PostgreSQL)
just test-oracle  # Run the suite against Oracle (starts Oracle Free; pure-Rust driver)
just ci           # fmt + lint + test (same as CI)
# ... and more
just -l     # List all recipes
```

## License

MIT
