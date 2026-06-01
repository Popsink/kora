<div align="center">

# Kora

**A Confluent-compatible Schema Registry, built in Rust.**

PostgreSQL storage · Single binary · Sub-millisecond lookups · Zero JVM overhead

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![ghcr.io](https://img.shields.io/badge/ghcr.io-Popsink%2Fkora-blue?logo=docker)](https://github.com/Popsink/kora/pkgs/container/kora)

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
    image: ghcr.io/Popsink/kora:latest
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
helm install kora oci://ghcr.io/Popsink/kora/charts/kora \
  --set database.host=my-postgres.example.com \
  --set database.password=secret
```

See [`chart/README.md`](chart/README.md) for all options.

### Docker

```bash
docker run -p 8080:8080 -e DATABASE_URL="postgres://user:pass@host:5432/kora" ghcr.io/Popsink/kora:latest
```

## Configuration

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | *(required)* | PostgreSQL connection string |
| `HOST` | `0.0.0.0` | Server bind address |
| `PORT` | `8080` | Server listen port |
| `MAX_BODY_SIZE` | `16777216` | Maximum request body size in bytes |
| `DB_POOL_MAX` | `20` | Maximum database connections |
| `RUST_LOG` | `info` | Log level (`error`, `warn`, `info`, `debug`, `trace`) |

## API

Kora implements the full [Confluent Schema Registry REST API](https://docs.confluent.io/platform/current/schema-registry/develop/api.html) — Avro, JSON Schema, and Protobuf with all 7 compatibility modes.

## Development

Requires [just](https://github.com/casey/just), [Rust](https://rustup.rs/), and [Docker](https://docs.docker.com/get-docker/).

```bash
just dev    # Run locally (starts PG via Docker Compose)
just test   # Run all tests
just ci     # fmt + lint + test (same as CI)
# ... and more
just -l     # List all recipes
```

## License

MIT
