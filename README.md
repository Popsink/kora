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

| `AUTH_ENABLED` | `false` | Require HTTP basic auth and enforce per-principal subject ACLs, see [Authentication](#authentication-and-multi-tenancy). Off by default: existing deployments keep working until they opt in |
| `KORA_ADMIN_USERNAME` / `KORA_ADMIN_PASSWORD` | *(unset)* | Bootstrap admin, reconciled on **every startup** (created, or promoted and its password reset when it differs). Set both or neither |
| `AUTH_CACHE_TTL_SECS` | `60` | How long a verified credential and a per-principal schema-ID read decision stay cached on a replica (`0` disables). Changes made through the admin API apply at once on the replica that served them, and on the others within this TTL |

Schema migrations run automatically on startup.

## API

Kora implements the full [Confluent Schema Registry REST API](https://docs.confluent.io/platform/current/schema-registry/develop/api.html) — Avro, JSON Schema, and Protobuf with all 7 compatibility modes.

## Authentication and multi-tenancy

With `AUTH_ENABLED=true`, every route except `/health` and `/metrics` requires
HTTP basic auth (what Confluent clients and the Kafka Connect converters send
with `basic.auth.credentials.source=USER_INFO` and
`basic.auth.user.info=<name>:<password>`). Missing or wrong credentials get
`401` (`error_code` 40101).

Principals live in PostgreSQL (`principals` table, Argon2id password hashes)
and have one of two roles:

- **`admin`**: everything, including global `/config` and `/mode` writes,
  per-subject `/mode` writes and the admin API;
- **`tenant`**: limited by a list of rules `{"pattern": …, "access": "read" | "read_write"}`.
  A pattern is an exact subject name (`orders-value`) or a prefix ending in `*`
  (`d1a2b3c4d5.*`; a lone `*` matches every subject).

| Routes | Rule for tenants |
|---|---|
| `/subjects/{subject}/**`, `DELETE /subjects/{subject}`, `/config/{subject}` | `GET` needs `read`, writes need `read_write` |
| `POST /subjects/{subject}` (lookup), `POST /compatibility/subjects/{subject}/**` | `read` (they do not modify anything) |
| `/mode/{subject}` | `GET` needs `read`; `PUT`/`DELETE` are admin-only |
| `POST /subjects/{subject}/versions` with `references` | also `read` on every referenced subject |
| `GET /subjects`, `GET /schemas` | filtered (in SQL) to the readable subjects, intersected with `subjectPrefix` |
| `GET /schemas/ids/{id}/subjects`, `/versions` | only the readable subject/version pairs; `404` if none |
| `GET /schemas/ids/{id}`, `/schemas/ids/{id}/schema` | `404` unless the ID is registered under at least one readable subject |
| `GET /config`, `GET /mode` (global) | any authenticated principal; writes are admin-only |
| `/health`, `/metrics` | no authentication |

A denied subject route returns `403` (`error_code` 40301). The decision only
looks at the rules, never at the database, so it does not reveal whether the
subject exists. Schema IDs are global and content-deduplicated: two tenants
registering the same schema share an ID and can both read it, but a tenant
cannot read an ID registered only under subjects it cannot see. That check is
cached per principal and ID for `AUTH_CACHE_TTL_SECS`.

### Admin API

Admin-only. Passwords are hashed before storage and never returned.

```bash
# Create a tenant (201)
curl -u "$ADMIN:$ADMIN_PW" -X POST localhost:8080/admin/principals \
  -H 'Content-Type: application/json' -d '{
    "name": "tenant-d1a2b3c4d5",
    "password": "…",
    "role": "tenant",
    "rules": [
      {"pattern": "d1a2b3c4d5.*", "access": "read_write"},
      {"pattern": "legacy-orders-value", "access": "read"}
    ]
  }'

curl -u "$ADMIN:$ADMIN_PW" localhost:8080/admin/principals                     # list
curl -u "$ADMIN:$ADMIN_PW" localhost:8080/admin/principals/tenant-d1a2b3c4d5   # get

# Rotate the password, change the role or replace the rules (any subset)
curl -u "$ADMIN:$ADMIN_PW" -X PATCH localhost:8080/admin/principals/tenant-d1a2b3c4d5 \
  -H 'Content-Type: application/json' -d '{"password": "…"}'

curl -u "$ADMIN:$ADMIN_PW" -X DELETE localhost:8080/admin/principals/tenant-d1a2b3c4d5   # 204
```

Errors: `404` / 40410 unknown principal, `409` / 40902 name taken, `422` /
42210 invalid name (empty or containing `:`), empty password or malformed rule.

### Observability

Request log lines carry a `principal` field (never the password). `401` and
`403` responses increment `kora_auth_rejections_total{status, principal}`; a
`401` is labelled with the attempted name only when that principal exists
(`unknown` otherwise, `anonymous` without credentials).

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
