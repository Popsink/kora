<div align="center">

# Kora

**A Confluent-compatible Schema Registry, built in Rust.**

PostgreSQL storage · Single binary · Sub-millisecond lookups · Zero JVM overhead

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![ghcr.io](https://img.shields.io/badge/ghcr.io-popsink%2Fkora-blue?logo=docker)](https://github.com/Popsink/kora/pkgs/container/kora)

</div>

## Why Kora?

| | Confluent | Karapace | Kora |
|---|---|---|---|
| **Storage** | Kafka topic | Kafka topic | PostgreSQL |
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

Images are published as `:<version>` / `:latest` (static musl, amd64 + arm64).
`:<version>-postgres` / `:latest-postgres` remain as aliases to the same digest
so existing pins keep working.

```bash
docker run -p 8080:8080 -e DATABASE_URL="postgres://user:pass@host:5432/kora" ghcr.io/popsink/kora:latest
```

## Configuration

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | *(required)* | PostgreSQL connection string (`postgres://…`). If empty, composed from the `DB_*` components below |
| `DB_HOST` / `DB_PORT` / `DB_USER` / `DB_PASSWORD` / `DB_NAME` | — | Connection components used when `DATABASE_URL` is empty (`DB_PORT` defaults to `5432`) |
| `HOST` | `0.0.0.0` | Server bind address |
| `PORT` | `8080` | Server listen port |
| `MAX_BODY_SIZE` | `16777216` | Maximum request body size in bytes |
| `DB_POOL_MAX` | `20` | Maximum database connections |
| `RUST_LOG` | `info` | Log level (`error`, `warn`, `info`, `debug`, `trace`) |
| `DEFAULT_COMPATIBILITY` | *(unset)* | Default global compatibility level, re-applied to the global config on **every startup** — overwriting any runtime `PUT`/`DELETE /config` change to the global level. One of `BACKWARD`, `BACKWARD_TRANSITIVE`, `FORWARD`, `FORWARD_TRANSITIVE`, `FULL`, `FULL_TRANSITIVE`, `NONE`. Leave unset (default) to let the runtime API own the level (`BACKWARD` on a fresh install) |

Schema migrations run automatically on startup.

## API

Kora implements the full [Confluent Schema Registry REST API](https://docs.confluent.io/platform/current/schema-registry/develop/api.html) — Avro, JSON Schema, and Protobuf with all 7 compatibility modes.

## Development

The toolchain is defined by [devbox](https://www.jetify.com/devbox) (see
[`devbox.json`](devbox.json) + [`rust-toolchain.toml`](rust-toolchain.toml)):
pinned Rust, `just`, `k6`, and `psql`. You also need
[Docker](https://docs.docker.com/get-docker/) for the database containers.

```bash
# Install devbox: https://www.jetify.com/docs/devbox/installing_devbox/
# In VS Code, the integrated terminal opens directly into `devbox shell` (see
# .vscode/settings.json), so the toolchain is ready to go.
# Elsewhere, run `devbox shell` once (or prefix a command with `devbox run --`).

just dev          # Run locally (starts PG via Docker Compose)
just test         # Run all tests
just ci           # fmt + lint + test (same as CI)
just -l           # List all recipes
```

## License

MIT
