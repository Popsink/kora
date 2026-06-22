---
title: Migrate from Karapace to Kora
description: Step-by-step guide to migrating your schema registry data from Karapace to Kora
---

# Migrate from Karapace to Kora

This guide walks you through migrating all schemas, subjects, versions, and compatibility configurations from a Karapace schema registry into Kora. The migration **preserves original Karapace schema IDs exactly** — your producers and consumers do not need to be reconfigured.

The process runs in four sequential phases: audit, dry-run, migrate, verify.

```mermaid
flowchart LR
    A[Karapace\ninstance] -->|just migrate-audit| B[Audit snapshot\nmigration/audits/*.json]
    B -->|just migrate-dry-run| C{Review\noutput}
    C -->|looks good| D[just migrate-run\nKora PostgreSQL]
    D -->|just migrate-verify| E[Kora\nverified]
    C -->|dedup collision| F[Resolve collision\nin Karapace]
    F --> B
```

| Phase | Command | What it does |
|-------|---------|--------------|
| 1. Audit | `just migrate-audit` | Snapshots all schemas, subjects, and configs from Karapace into a local JSON file |
| 2. Dry run | `just migrate-dry-run` | Simulates the migration — prints every action without writing to the database |
| 3. Migrate | `just migrate-run` | Writes schemas directly into Kora's PostgreSQL, preserving all original IDs |
| 4. Verify | `just migrate-verify` | Validates every schema ID, subject, and version in Kora against the audit snapshot |

---

## Prerequisites

### Tools

| Tool | Version | Install |
|------|---------|---------|
| [`just`](https://just.systems) | 1.x | `brew install just` or see [installer](https://just.systems/man/en/chapter_4.html) |
| [`uv`](https://docs.astral.sh/uv/) | 0.4+ | `curl -LsSf https://astral.sh/uv/install.sh \| sh` |
| Python | 3.11+ | Managed automatically by `uv` — no manual install needed |

`uv` manages the Python virtual environment and dependencies (`fastavro`, `psycopg2-binary`) automatically on first run. You do not need to run `pip install` manually.

### Access requirements

Before starting, make sure you have:

- **HTTP/HTTPS access** to your Karapace instance (for the audit and to resolve any collisions)
- **Direct PostgreSQL access** to your Kora database (TCP, port 5432 by default) — required for the migration step
- **HTTP/HTTPS access** to your Kora instance (for the verification step)

### Kora database must be empty

> **Warning:** `migrate-run` requires a completely empty Kora database (zero rows in `schema_contents`). It will refuse to run and exit with an error if any schemas already exist. Run the migration on a fresh Kora instance only.

---

## Configuration

All scripts read connection details from **environment variables**. The `justfile` loads a `.env` file from the project root automatically (`set dotenv-load`), so the recommended approach is to create a `.env` file once and reuse it across all phases.

### Setting up `.env`

Create a `.env` file in the root of this repository:

```bash
# Karapace source (required for audit)
KARAPACE_URL=https://karapace.example.com
KARAPACE_USER=your-karapace-user       # omit if no auth
KARAPACE_PASSWORD=your-karapace-pass   # omit if no auth

# Kora target database (required for migrate)
KORA_DB_URL=postgresql://kora:secret@kora-db.example.com:5432/kora

# Kora API (required for verify)
KORA_URL=https://kora.example.com
KORA_USER=your-kora-user               # omit if no auth
KORA_PASSWORD=your-kora-pass           # omit if no auth

# Audit file (optional — defaults to the latest file in migration/audits/)
# AUDIT_FILE=migration/audits/karapace.example.com-2024-01-15T120000Z.json
```

> **Note:** If your Karapace or Kora instance does not require authentication, omit the `*_USER` and `*_PASSWORD` variables entirely.

---

## Step 1 — Audit Karapace

```bash
just migrate-audit
```

This connects to your Karapace instance and fetches every subject, version, schema, and per-subject compatibility configuration — including soft-deleted subjects and versions. It writes the result to a timestamped JSON file under `migration/audits/`.

Fetching is parallelised across subjects (20 concurrent workers by default), so auditing a large registry typically completes in seconds.

**Example output:**

```
Auditing https://karapace.example.com ...
  Global config: {"compatibilityLevel": "BACKWARD"}
  Subjects: 42 total (3 soft-deleted)
  Fetching with 20 workers ...
  [1/42] orders-value
  [2/42] payments-key
  ...
  No dedup collisions found.

Audit complete. Written to migration/audits/karapace.example.com-2024-01-15T120000Z.json
```

The audit also prints a JSON summary to stdout:

```json
{
  "timestamp": "2024-01-15T12:00:00+00:00",
  "source_url": "https://karapace.example.com",
  "subject_count": 42,
  "subject_count_active": 39,
  "subject_count_deleted": 3,
  "schema_count": 58,
  "version_count": 97,
  "has_references": false,
  "dedup_collision_count": 0,
  "global_config": { "compatibilityLevel": "BACKWARD" }
}
```

Keep this file — it is the source of truth for all subsequent phases.

---

## Step 2 — Check for dedup collisions

Before moving to the dry run, look at `dedup_collision_count` in the audit summary.

### What is a dedup collision?

Karapace assigns a new schema ID every time a schema is registered, even if the content is byte-for-byte identical to an existing schema. Kora uses **content-based deduplication** (via schema fingerprinting), so two subjects sharing identical schema content would be collapsed into a single ID — breaking the ID-preservation guarantee.

The audit script detects this automatically and reports it as a **dedup collision**.

### If `dedup_collision_count` is 0

No action needed. Proceed to [Step 3](#step-3--dry-run).

### If `dedup_collision_count` is greater than 0

The migration will **refuse to run** until collisions are resolved. The audit JSON includes a `dedup_collisions` array identifying the conflicting schemas:

```json
"dedup_collisions": [
  {
    "canonical_content": "{\"type\":\"record\",\"name\":\"Order\",...}",
    "ids": [12, 47]
  }
]
```

This means schema ID 12 and schema ID 47 in Karapace contain the same schema content. To resolve:

1. Identify which subjects reference each conflicting ID (check the `schemas_by_id[id].subject_versions` array in the audit file).
2. In Karapace, consolidate the affected subjects so they all reference the same schema ID — typically by re-registering one subject under the other's schema version, then deleting the duplicate.
3. Re-run `just migrate-audit` to produce a fresh snapshot.
4. Confirm `dedup_collision_count` is now `0`.

> **Note:** If consolidating the subjects is not straightforward, contact [Popsink support](mailto:support@popsink.com) — they can advise on the safest resolution strategy for your topology.

---

## Step 3 — Dry run

```bash
just migrate-dry-run
```

This reads the latest audit snapshot and prints every database operation that _would_ be executed, without writing a single row. Use it to confirm counts and spot any obvious issues before touching the database.

**Example output:**

```
Source  : karapace.example.com (audit from 2024-01-15T12:00:00+00:00)
Target  : kora-db.example.com:5432/kora
[DRY RUN — no writes]

  Schemas  : 58
  Subjects : 42
  Versions : 97

Step 1/5: Inserting schema_contents ...
  [dry-run] Would insert 58 schema_contents rows.
Step 2/5: Inserting subjects ...
  [dry-run] Would insert 42 subject rows.
Step 3/5: Inserting schema_versions ...
  [dry-run] Would insert 97 schema_version rows.
Step 4/5: Inserting configs ...
  [dry-run] Would set global compatibility → BACKWARD
  [dry-run] Would insert 5 per-subject config row(s).
Step 5/5: Resetting sequences ...
  [dry-run] Would reset sequences.

Dry run complete — no changes made.
```

Verify that the schema, subject, and version counts match your audit summary before proceeding.

---

## Step 4 — Run the migration

```bash
just migrate-run
```

This writes all migrated data directly into Kora's PostgreSQL in a **single transaction**. If any step fails, the entire transaction is rolled back — your database is left unchanged.

The migration runs five steps in order:

| Step | What it writes |
|------|---------------|
| 1/5 | `schema_contents` — all schema texts with their **original Karapace IDs** (explicit `INSERT` with ID, bypassing the auto-increment sequence) |
| 2/5 | `subjects` — all subject names, including soft-deleted subjects |
| 3/5 | `schema_versions` — all version → schema ID mappings, including soft-deleted versions |
| 4/5 | `config` — global compatibility level and per-subject overrides |
| 5/5 | Sequences — resets the PostgreSQL `BIGSERIAL` sequences to the current `MAX(id)` so future registrations continue from the right value |

**Example output:**

```
Source  : karapace.example.com (audit from 2024-01-15T12:00:00+00:00)
Target  : kora-db.example.com:5432/kora

  Schemas  : 58
  Subjects : 42
  Versions : 97

Step 1/5: Inserting schema_contents ...
  Inserted 58 schema_contents rows.
Step 2/5: Inserting subjects ...
  Inserted 42 subject rows.
Step 3/5: Inserting schema_versions ...
  Inserted 97 schema_version rows.
Step 4/5: Inserting configs ...
  Global compatibility → BACKWARD
  Inserted 5 per-subject config row(s).
Step 5/5: Resetting sequences ...
  schema_contents_id_seq → 58
  subjects_id_seq → 42
  schema_versions_id_seq → 97

Migration committed successfully.
```

---

## Step 5 — Verify

```bash
just migrate-verify
```

This connects to your live Kora instance (via HTTP, not directly to PostgreSQL) and runs three checks against the audit snapshot:

| Check | What it validates |
|-------|-------------------|
| Schema IDs | Every schema ID from Karapace resolves in Kora to the correct schema content |
| Subject versions | Every subject/version pair maps to the expected schema ID and content |
| Subject list | The set of active subjects in Kora matches the active subjects in the audit |

**Success output:**

```
Verifying https://kora.example.com against audit from 2024-01-15T12:00:00+00:00
Expecting 58 schema(s) across 42 subject(s)

Checking schema IDs ...
  [ 1] ok
  [ 2] ok
  ...
Checking subject versions ...
  orders-value v1 → id=1 ok
  ...
Checking subject list ...
  39 subject(s) — matches audit

============================================================
Checks run : 156
Failures   : 0

All checks passed — migration verified successfully.
```

If any check fails, the script exits with a non-zero status and prints each failure — for example:

```
FAILURES:
  ID 47: GET /schemas/ids/47 → Schema not found.
  orders-value v2: ID mismatch — expected 47, got 12
```

Do not route production traffic to Kora until `migrate-verify` passes with zero failures.

---

## Environment variables reference

| Variable | Used by | Description |
|----------|---------|-------------|
| `KARAPACE_URL` | `migrate-audit` | Base URL of the source Karapace instance, e.g. `https://karapace.example.com` |
| `KARAPACE_USER` | `migrate-audit` | BasicAuth username for Karapace (optional) |
| `KARAPACE_PASSWORD` | `migrate-audit` | BasicAuth password for Karapace (optional) |
| `KORA_DB_URL` | `migrate-dry-run`, `migrate-run` | PostgreSQL connection URL, e.g. `postgresql://user:pass@host:5432/dbname` |
| `KORA_URL` | `migrate-verify` | Base URL of the target Kora instance, e.g. `https://kora.example.com` |
| `KORA_USER` | `migrate-verify` | BasicAuth username for Kora (optional) |
| `KORA_PASSWORD` | `migrate-verify` | BasicAuth password for Kora (optional) |
| `AUDIT_FILE` | `migrate-dry-run`, `migrate-run`, `migrate-verify` | Path to the audit JSON file. Defaults to the most recently modified file in `migration/audits/` |

---

## Troubleshooting

### `ERROR: N dedup collision(s) in audit`

The migration script detected identical schema content assigned to different IDs in Karapace. See [Step 2 — Check for dedup collisions](#step-2--check-for-dedup-collisions) for the resolution process.

### `ERROR: schema_contents is not empty (N row(s) exist)`

The target Kora database already has schema data. This migration tool is designed for **initial population only**. If you need to re-run the migration, restore the database to a clean state first (e.g. drop and recreate the schema, then re-run Kora's database migrations).

### `No audit file found in audits/`

Either `just migrate-audit` has not been run yet, or `AUDIT_FILE` points to a path that does not exist. Run `just migrate-audit` first, or set `AUDIT_FILE` explicitly in your `.env`.

### Connection refused / timeout on Karapace or Kora

Verify that:
- The URL in `KARAPACE_URL` / `KORA_URL` is reachable from the machine running the migration
- Any firewall or VPN rules allow outbound HTTP/HTTPS to those hosts
- Credentials in `*_USER` / `*_PASSWORD` are correct (test with `curl -u user:pass <url>/subjects`)

### Connection refused on Kora PostgreSQL

Verify that:
- `KORA_DB_URL` uses the correct host, port, database name, and credentials
- The PostgreSQL instance allows connections from your IP (check `pg_hba.conf`)
- The database user has `INSERT` and `UPDATE` privileges on the Kora schema tables
