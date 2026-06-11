# Kora — Schema Registry API Reference Catalogue

> **📌 Scope note (read first):** this catalogue covers **Kora in isolation** — its
> Schema Registry REST API. It largely overlaps the developer's own unit/integration
> tests, so it is kept as a **reference annex**, not the primary QA deliverable. The
> primary e2e work is the **Kora ↔ Kotatsu integration** suite in
> [`kotatsu-integration.md`](./kotatsu-integration.md), which tests the live chain from
> the outside (the layer unit tests don't cover). Use this file to look up Kora's
> endpoints and error codes; use `kotatsu-integration.md` to test what users actually see.

> **Status:** Living document — functional specification of expected behaviour, written
> first; automation comes later (see issue *"QA: end-to-end test case catalogue"*).
>
> **Scope:** the Schema Registry REST API exposed by Kora, tested end-to-end (real
> server + real PostgreSQL, exercised over HTTP). This describes *what* Kora must do —
> it is independent of any test framework or script.
>
> **Source of truth:** Kora claims 100% wire-compatibility with the
> [Confluent Schema Registry REST API](https://docs.confluent.io/platform/current/schema-registry/develop/api.html).
> Expected results below are derived from Kora's own handlers (`src/api/*.rs`,
> `src/error.rs`) cross-checked against the Confluent spec.

## How to read this document

Each test case has a stable ID: `KORA-<DOMAIN>-<NNN>`.

| Field | Meaning |
|---|---|
| **ID** | Stable identifier, never reused. |
| **Title** | One-line intent. |
| **Priority** | `P1` critical (core path / product promise) · `P2` important · `P3` edge/nice-to-have. |
| **Preconditions** | State the registry must be in before the steps. |
| **Steps** | Ordered HTTP actions. |
| **Test data** | Schemas / payloads used. |
| **Expected result** | HTTP status, body shape, and any side effect. |

### Conventions used across cases

- **Base URL** `{base}` — a running Kora instance (random port in tests).
- **Content-Type** for all API responses: `application/vnd.schemaregistry.v1+json`.
- **Subject naming** — every test MUST use a unique subject (e.g. suffix a UUID).
  Kora dedups schema *content* globally by fingerprint, so reusing the same subject
  or schema text across tests causes cross-contamination. Use unique record names
  for isolation (cf. `tests/common/mod.rs::unique_avro_schema`).
- **Error body shape** — all errors return `{ "error_code": <int>, "message": <string> }`.

### Confluent error code reference (as implemented in `src/error.rs`)

| Code | HTTP | Meaning |
|---|---|---|
| 40401 | 404 | Subject not found |
| 40402 | 404 | Version not found |
| 40403 | 404 | Schema not found |
| 40404 | 404 | Subject was soft-deleted (use `permanent=true`) |
| 40405 | 404 | Subject not soft-deleted first (hard-delete precondition) |
| 40406 | 404 | Version was soft-deleted |
| 40407 | 404 | Version not soft-deleted first (hard-delete precondition) |
| 40408 | 404 | Subject has no subject-level compatibility configured |
| 40409 | 404 | Subject has no subject-level mode configured |
| 40901 | 409 | Schema incompatible with an earlier version |
| 42201 | 422 | Invalid schema / reference not found |
| 42202 | 422 | Invalid version |
| 42203 | 422 | Invalid compatibility level |
| 42204 | 422 | Invalid mode |
| 42205 | 422 | Operation not permitted (read-only mode) |
| 42206 | 422 | Schema is referenced and cannot be deleted |
| 50001 | 500 | Backend data store error |

---

## Domain coverage map

| # | Domain | Prefix | Cases | Existing automation |
|---|---|---|---|---|
| 1 | Schema registration | `REG` | 12 | `tests/api_register_schema.rs` |
| 2 | Schema retrieval | `GET` | 11 | `tests/api_get_schema_by_id.rs`, `api_get_schema_by_version.rs` |
| 3 | Compatibility checking | `CMP` | 12 | `tests/api_compatibility_test.rs`, `confluent_*_compat.rs` |
| 4 | Configuration | `CFG` | 9 | `tests/api_compatibility_config.rs` |
| 5 | Mode | `MOD` | 9 | `tests/api_mode.rs` |
| 6 | Deletion | `DEL` | 13 | `tests/api_delete_subject.rs`, `api_hard_delete.rs` |
| 7 | Cross-references | `REF` | 8 | `tests/api_schema_references.rs`, `api_schema_cross_refs.rs` |
| 8 | Confluent wire-compatibility | `WIRE` | 7 | `tests/api_root.rs`, `error_responses.rs` |
| 9 | Robustness / error handling | `ERR` | 10 | `tests/error_responses.rs`, `config.rs` |
| — | Listing & discovery | `LST` | 7 | `tests/api_list_subjects.rs`, `api_list_schemas.rs`, `api_list_schema_types.rs` |
| — | Health & metrics | `OPS` | 5 | `tests/api_health.rs`, `api_metrics.rs` |

---

## 1. Schema registration — `POST /subjects/{subject}/versions`

| ID | Title | Pri |
|---|---|---|
| KORA-REG-001 | Register first Avro schema returns id + version 1 | P1 |
| KORA-REG-002 | Register same schema text again is idempotent (same id, same version) | P1 |
| KORA-REG-003 | Register a backward-compatible evolution creates version 2 | P1 |
| KORA-REG-004 | Register JSON Schema with explicit `schemaType: JSON` | P1 |
| KORA-REG-005 | Register Protobuf with explicit `schemaType: PROTOBUF` | P1 |
| KORA-REG-006 | Missing `schemaType` defaults to AVRO | P2 |
| KORA-REG-007 | Identical schema text under two different subjects shares the same global id | P2 |
| KORA-REG-008 | Register a syntactically invalid schema is rejected (42201) | P1 |
| KORA-REG-009 | Wrong `schemaType` for the payload is rejected (42201) | P2 |
| KORA-REG-010 | Register response always includes id, version, schemaType, schema | P2 |
| KORA-REG-011 | Register on empty subject name is rejected (42201) | P2 |
| KORA-REG-012 | Re-register a previously soft-deleted schema (resurrection behaviour) | P2 |

### KORA-REG-001 — Register first Avro schema
- **Priority:** P1
- **Preconditions:** Subject `s` does not exist.
- **Steps:**
  1. `POST {base}/subjects/{s}/versions` with body `{"schema": <AVRO_V1>}`.
- **Test data:** `AVRO_V1 = {"type":"record","name":"T<uuid>","fields":[{"name":"id","type":"int"}]}`
- **Expected:** `200 OK`. Body contains `id` (positive integer), `version: 1`,
  `schemaType: "AVRO"`, and `schema` echoing the input. Subject `s` now appears in `GET /subjects`.

### KORA-REG-002 — Re-register identical schema is idempotent
- **Priority:** P1
- **Preconditions:** KORA-REG-001 executed; `s` has version 1 with id `X`.
- **Steps:**
  1. `POST {base}/subjects/{s}/versions` with the **exact same** schema text.
- **Expected:** `200 OK`, `id == X`, `version == 1`. No new version row created.
  `GET /subjects/{s}/versions` still returns `[1]`.

### KORA-REG-003 — Backward-compatible evolution → version 2
- **Priority:** P1
- **Preconditions:** `s` has V1; global compatibility is the default `BACKWARD`.
- **Steps:**
  1. Register `AVRO_V2` (adds optional field with `default: null`) under `s`.
- **Test data:** `AVRO_V2` = V1 + `{"name":"name","type":["null","string"],"default":null}`
- **Expected:** `200 OK`, `version: 2`, new `id`. `GET /subjects/{s}/versions` → `[1,2]`.

### KORA-REG-004 — Register JSON Schema
- **Priority:** P1
- **Steps:** `POST .../versions` with `{"schema": <JSON_SCHEMA>, "schemaType": "JSON"}`.
- **Test data:** `{"type":"object","properties":{"name":{"type":"string"}}}`
- **Expected:** `200 OK`, `schemaType: "JSON"`, `version: 1`.

### KORA-REG-005 — Register Protobuf
- **Priority:** P1
- **Steps:** `POST .../versions` with `{"schema": "syntax=\"proto3\";message T{int32 id=1;}", "schemaType": "PROTOBUF"}`.
- **Expected:** `200 OK`, `schemaType: "PROTOBUF"`, `version: 1`.

### KORA-REG-006 — Missing schemaType defaults to AVRO
- **Priority:** P2
- **Steps:** Register a valid Avro schema **without** `schemaType`.
- **Expected:** `200 OK`, response `schemaType: "AVRO"`.

### KORA-REG-007 — Same text across subjects shares global id
- **Priority:** P2
- **Preconditions:** None.
- **Steps:** Register identical schema text under subject `a` and subject `b`.
- **Expected:** Both return the **same** `id`. Both subjects independently report `version: 1`.

### KORA-REG-008 — Invalid schema rejected
- **Priority:** P1
- **Steps:** Register `{"schema": "{ this is not valid avro }"}`.
- **Expected:** `422`, `error_code: 42201`, message describing the parse failure.

### KORA-REG-009 — Type/payload mismatch rejected
- **Priority:** P2
- **Steps:** Register a Protobuf body with `schemaType: "AVRO"` (or vice-versa).
- **Expected:** `422`, `error_code: 42201`.

### KORA-REG-010 — Response shape contract
- **Priority:** P2
- **Steps:** Register any valid schema.
- **Expected:** Response JSON has exactly the keys `id`, `version`, `schemaType`, `schema`
  (and `references` only when references were supplied — see Domain 7).

### KORA-REG-011 — Empty subject name rejected
- **Priority:** P2
- **Steps:** `POST {base}/subjects//versions` (empty subject segment).
- **Expected:** `422`, `error_code: 42201`, message "Subject name must not be empty".

### KORA-REG-012 — Re-register after soft-delete
- **Priority:** P2
- **Preconditions:** `s` v1 exists, then soft-deleted (Domain 6).
- **Steps:** Register a schema under `s` again.
- **Expected:** Documented and consistent behaviour (subject reactivates / new version
  assigned per Confluent semantics). **Verify** the exact version number returned and
  that the subject is active again in `GET /subjects`.

---

## 2. Schema retrieval

| ID | Title | Pri |
|---|---|---|
| KORA-GET-001 | Get schema by global id | P1 |
| KORA-GET-002 | Get schema by id — not found (40403) | P1 |
| KORA-GET-003 | Get schema by subject + explicit version | P1 |
| KORA-GET-004 | Get schema by subject + `latest` | P1 |
| KORA-GET-005 | Get schema — unknown subject (40401) | P1 |
| KORA-GET-006 | Get schema — unknown version on existing subject (40402) | P1 |
| KORA-GET-007 | Get raw schema text (`.../schema` endpoint, no metadata wrapper) | P2 |
| KORA-GET-008 | Get raw schema text by id (`/schemas/ids/{id}/schema`) | P2 |
| KORA-GET-009 | Invalid version string (e.g. `abc`, `0`, `-1`) → 42202 | P1 |
| KORA-GET-010 | Get a soft-deleted version only with `?deleted=true` | P2 |
| KORA-GET-011 | `GET /schemas/ids/{id}/subjects` lists subjects for a schema | P2 |

### KORA-GET-001 — Get by global id
- **Priority:** P1
- **Preconditions:** A schema registered with id `X`.
- **Steps:** `GET {base}/schemas/ids/{X}`.
- **Expected:** `200 OK`, body contains `schema` (and `schemaType` when not Avro).

### KORA-GET-002 — Get by id, not found
- **Priority:** P1
- **Steps:** `GET {base}/schemas/ids/999999999`.
- **Expected:** `404`, `error_code: 40403`.

### KORA-GET-003 — Get by subject + version
- **Priority:** P1
- **Preconditions:** `s` has versions 1 and 2.
- **Steps:** `GET {base}/subjects/{s}/versions/1`.
- **Expected:** `200 OK`, body `{ subject, id, version: 1, schema, ... }`.

### KORA-GET-004 — Get `latest`
- **Priority:** P1
- **Preconditions:** `s` has versions 1 and 2.
- **Steps:** `GET {base}/subjects/{s}/versions/latest`.
- **Expected:** `200 OK`, `version: 2`.

### KORA-GET-005 — Unknown subject
- **Priority:** P1
- **Steps:** `GET {base}/subjects/does-not-exist/versions/1`.
- **Expected:** `404`, `error_code: 40401`.

### KORA-GET-006 — Unknown version on existing subject
- **Priority:** P1
- **Preconditions:** `s` has only version 1.
- **Steps:** `GET {base}/subjects/{s}/versions/99`.
- **Expected:** `404`, `error_code: 40402`. (Distinct from 40401 — verify the code differs
  between "no subject" and "subject exists but no such version".)

### KORA-GET-007 — Raw schema text by version
- **Priority:** P2
- **Steps:** `GET {base}/subjects/{s}/versions/1/schema`.
- **Expected:** `200 OK`, body is the raw schema string only (no `id`/`version` wrapper).

### KORA-GET-008 — Raw schema text by id
- **Priority:** P2
- **Steps:** `GET {base}/schemas/ids/{X}/schema`.
- **Expected:** `200 OK`, raw schema string only.

### KORA-GET-009 — Invalid version string
- **Priority:** P1
- **Steps:** `GET {base}/subjects/{s}/versions/abc`, then `/0`, then `/-1`.
- **Expected:** each `422`, `error_code: 42202`. (Only positive integers and `latest` are valid.)

### KORA-GET-010 — Soft-deleted version visibility
- **Priority:** P2
- **Preconditions:** `s` v2 soft-deleted.
- **Steps:** `GET .../versions/2` (expect 40402), then `GET .../versions/2?deleted=true` (expect 200).
- **Expected:** Hidden by default, visible with `deleted=true`.

### KORA-GET-011 — Subjects for a schema id
- **Priority:** P2
- **Preconditions:** Same schema text registered under `a` and `b` (shared id `X`).
- **Steps:** `GET {base}/schemas/ids/{X}/subjects`.
- **Expected:** `200 OK`, array containing both `a` and `b`.

---

## 3. Compatibility checking

> Default global level is `BACKWARD`. Valid levels: `BACKWARD`, `BACKWARD_TRANSITIVE`,
> `FORWARD`, `FORWARD_TRANSITIVE`, `FULL`, `FULL_TRANSITIVE`, `NONE`.
> Test endpoints: `POST /compatibility/subjects/{s}/versions/{version}` and
> `.../versions` (all versions). Response: `{ "is_compatible": bool }`, plus
> `{ "messages": [...] }` when `?verbose=true`.

| ID | Title | Pri |
|---|---|---|
| KORA-CMP-001 | BACKWARD: adding optional field is compatible | P1 |
| KORA-CMP-002 | BACKWARD: adding required field (no default) is incompatible | P1 |
| KORA-CMP-003 | Registration blocked when new schema is incompatible (40901) | P1 |
| KORA-CMP-004 | NONE level allows any schema change | P1 |
| KORA-CMP-005 | FORWARD: removing optional field semantics | P2 |
| KORA-CMP-006 | FULL: must satisfy both backward and forward | P2 |
| KORA-CMP-007 | TRANSITIVE checks against all prior versions, not just latest | P1 |
| KORA-CMP-008 | Test against `latest` vs explicit version | P2 |
| KORA-CMP-009 | Test against all versions endpoint | P2 |
| KORA-CMP-010 | `verbose=true` returns explanatory messages | P2 |
| KORA-CMP-011 | Compatibility test against nonexistent subject → is_compatible: true | P2 |
| KORA-CMP-012 | Schema-type mismatch in compat test rejected (42201) | P2 |

### KORA-CMP-001 — BACKWARD adds optional field
- **Priority:** P1
- **Preconditions:** `s` v1 = `COMPAT_AVRO_V1`; level BACKWARD.
- **Steps:** `POST /compatibility/subjects/{s}/versions/latest` with `COMPAT_AVRO_V2`.
- **Expected:** `200 OK`, `is_compatible: true`.

### KORA-CMP-002 — BACKWARD rejects new required field
- **Priority:** P1
- **Preconditions:** `s` v1 = `COMPAT_AVRO_V1`; level BACKWARD.
- **Steps:** Compat-test `COMPAT_AVRO_INCOMPAT` (adds required `email`, no default).
- **Expected:** `200 OK`, `is_compatible: false`.

### KORA-CMP-003 — Registration blocked on incompatibility
- **Priority:** P1
- **Preconditions:** `s` v1 exists; level BACKWARD.
- **Steps:** `POST /subjects/{s}/versions` with `COMPAT_AVRO_INCOMPAT`.
- **Expected:** `409`, `error_code: 40901`. No version 2 created.

### KORA-CMP-004 — NONE allows anything
- **Priority:** P1
- **Preconditions:** `s` v1 exists; set subject level to `NONE`.
- **Steps:** Register `COMPAT_AVRO_INCOMPAT` under `s`.
- **Expected:** `200 OK`, `version: 2` (no compat check performed).

### KORA-CMP-005 — FORWARD semantics
- **Priority:** P2
- **Preconditions:** level FORWARD.
- **Steps:** Verify a change that is forward-compatible passes and a forward-breaking change fails.
- **Expected:** Results match Confluent FORWARD rules (new schema can read data written by old).

### KORA-CMP-006 — FULL semantics
- **Priority:** P2
- **Preconditions:** level FULL.
- **Steps:** A change compatible in only one direction must fail; a fully-compatible change passes.
- **Expected:** `is_compatible: false` for one-directional change.

### KORA-CMP-007 — TRANSITIVE checks all versions
- **Priority:** P1
- **Preconditions:** `s` has v1, v2; level `BACKWARD_TRANSITIVE`.
- **Steps:** Register a schema compatible with v2 but **incompatible with v1**.
- **Expected:** `409`, `40901` (transitive walks the full history, not just latest).
  Contrast: under plain `BACKWARD` the same schema would succeed.

### KORA-CMP-008 — `latest` vs explicit version
- **Priority:** P2
- **Steps:** Compat-test the same candidate against `latest` and against an older version.
- **Expected:** Results consistent with which version is targeted.

### KORA-CMP-009 — All-versions endpoint
- **Priority:** P2
- **Steps:** `POST /compatibility/subjects/{s}/versions` (no version segment).
- **Expected:** `200 OK`, aggregate `is_compatible` reflecting all versions.

### KORA-CMP-010 — Verbose messages
- **Priority:** P2
- **Steps:** Compat-test an incompatible schema with `?verbose=true`.
- **Expected:** `is_compatible: false` and non-empty `messages` array describing the break.

### KORA-CMP-011 — Nonexistent subject is compatible
- **Priority:** P2
- **Steps:** `POST /compatibility/subjects/never-seen/versions` with any schema.
- **Expected:** `200 OK`, `is_compatible: true` (no versions to violate).

### KORA-CMP-012 — Type mismatch in compat test
- **Priority:** P2
- **Preconditions:** `s` v1 is Avro.
- **Steps:** Compat-test a Protobuf schema against `s`.
- **Expected:** `422`, `error_code: 42201`, message mentions schema type mismatch.

---

## 4. Configuration (compatibility) — `/config` and `/config/{subject}`

| ID | Title | Pri |
|---|---|---|
| KORA-CFG-001 | Default global compatibility is BACKWARD | P1 |
| KORA-CFG-002 | Set global compatibility level | P1 |
| KORA-CFG-003 | Set per-subject compatibility overrides global | P1 |
| KORA-CFG-004 | Get subject config with no override → 40408 | P2 |
| KORA-CFG-005 | Get subject config with `defaultToGlobal=true` falls back to global | P2 |
| KORA-CFG-006 | Invalid compatibility level rejected (42203) | P1 |
| KORA-CFG-007 | Delete per-subject config returns previous level | P2 |
| KORA-CFG-008 | Delete global config resets to BACKWARD, returns previous | P2 |
| KORA-CFG-009 | `normalize` flag persisted and echoed | P3 |

### KORA-CFG-001 — Default global level
- **Priority:** P1
- **Steps:** `GET {base}/config`.
- **Expected:** `200 OK`, `compatibilityLevel: "BACKWARD"`, `normalize: false`.

### KORA-CFG-002 — Set global level
- **Priority:** P1
- **Steps:** `PUT {base}/config` `{"compatibility": "FULL"}`, then `GET {base}/config`.
- **Expected:** PUT `200`, body `compatibility: "FULL"`. GET reflects `FULL`.

### KORA-CFG-003 — Subject override
- **Priority:** P1
- **Preconditions:** global `BACKWARD`.
- **Steps:** `PUT {base}/config/{s}` `{"compatibility": "NONE"}`, then register an
  otherwise-incompatible schema under `s`.
- **Expected:** Subject-level `NONE` wins → registration succeeds despite global BACKWARD.

### KORA-CFG-004 — Subject config not configured
- **Priority:** P2
- **Steps:** `GET {base}/config/{s}` where `s` has no override (and no `defaultToGlobal`).
- **Expected:** `404`, `error_code: 40408`.

### KORA-CFG-005 — defaultToGlobal fallback
- **Priority:** P2
- **Steps:** `GET {base}/config/{s}?defaultToGlobal=true` with no per-subject config.
- **Expected:** `200 OK`, returns the global level.

### KORA-CFG-006 — Invalid level rejected
- **Priority:** P1
- **Steps:** `PUT {base}/config` `{"compatibility": "SIDEWAYS"}`.
- **Expected:** `422`, `error_code: 42203`.

### KORA-CFG-007 — Delete subject config
- **Priority:** P2
- **Preconditions:** `s` has subject-level `FULL`.
- **Steps:** `DELETE {base}/config/{s}`.
- **Expected:** `200 OK`, returns previous `compatibilityLevel: "FULL"`. Subsequent
  `GET /config/{s}` (no defaultToGlobal) → 40408.

### KORA-CFG-008 — Delete global config
- **Priority:** P2
- **Preconditions:** global set to `FORWARD`.
- **Steps:** `DELETE {base}/config`.
- **Expected:** `200 OK`, returns previous level `FORWARD`. `GET /config` then shows `BACKWARD`.

### KORA-CFG-009 — normalize flag
- **Priority:** P3
- **Steps:** `PUT {base}/config` `{"compatibility":"BACKWARD","normalize":true}`, then GET.
- **Expected:** `normalize: true` persisted and echoed.

---

## 5. Mode — `/mode` and `/mode/{subject}`

> Valid modes: `READWRITE`, `READONLY`, `READONLY_OVERRIDE`, `IMPORT`, `FORWARD`.
> Write-permitting modes: `READWRITE`, `IMPORT`, `FORWARD`, `READONLY_OVERRIDE`.
> Default global mode is `READWRITE`.

| ID | Title | Pri |
|---|---|---|
| KORA-MOD-001 | Default global mode is READWRITE | P1 |
| KORA-MOD-002 | Set global mode to READONLY blocks registration (42205) | P1 |
| KORA-MOD-003 | READONLY blocks deletion (42205) | P1 |
| KORA-MOD-004 | Per-subject mode overrides global | P1 |
| KORA-MOD-005 | Invalid mode rejected (42204) | P1 |
| KORA-MOD-006 | Get subject mode with no override → 40409 | P2 |
| KORA-MOD-007 | Delete global mode resets to READWRITE | P2 |
| KORA-MOD-008 | Delete per-subject mode → falls back to global | P2 |
| KORA-MOD-009 | READONLY still allows reads | P1 |

### KORA-MOD-001 — Default mode
- **Priority:** P1
- **Steps:** `GET {base}/mode`.
- **Expected:** `200 OK`, `{"mode": "READWRITE"}`.

### KORA-MOD-002 — READONLY blocks registration
- **Priority:** P1
- **Steps:** `PUT {base}/mode` `{"mode":"READONLY"}`, then `POST /subjects/{s}/versions`.
- **Expected:** registration → `422`, `error_code: 42205` (Operation not permitted).
  **Teardown:** reset to READWRITE.

### KORA-MOD-003 — READONLY blocks deletion
- **Priority:** P1
- **Preconditions:** `s` v1 exists; global mode READONLY.
- **Steps:** `DELETE {base}/subjects/{s}`.
- **Expected:** `422`, `error_code: 42205`.

### KORA-MOD-004 — Per-subject override
- **Priority:** P1
- **Preconditions:** global READWRITE.
- **Steps:** `PUT {base}/mode/{s}` `{"mode":"READONLY"}`, then register under `s`.
- **Expected:** registration on `s` → `42205`; registration on a *different* subject still succeeds.

### KORA-MOD-005 — Invalid mode
- **Priority:** P1
- **Steps:** `PUT {base}/mode` `{"mode":"SLEEPING"}`.
- **Expected:** `422`, `error_code: 42204`.

### KORA-MOD-006 — Subject mode not configured
- **Priority:** P2
- **Steps:** `GET {base}/mode/{s}` with no per-subject mode and no `defaultToGlobal`.
- **Expected:** `404`, `error_code: 40409`.

### KORA-MOD-007 — Delete global mode
- **Priority:** P2
- **Preconditions:** global set to READONLY.
- **Steps:** `DELETE {base}/mode`.
- **Expected:** `200 OK`, returns previous mode; `GET /mode` → READWRITE.

### KORA-MOD-008 — Delete subject mode
- **Priority:** P2
- **Preconditions:** `s` has subject mode READONLY; global READWRITE.
- **Steps:** `DELETE {base}/mode/{s}`, then register under `s`.
- **Expected:** `200 OK` on delete; registration now succeeds (falls back to global READWRITE).

### KORA-MOD-009 — READONLY allows reads
- **Priority:** P1
- **Preconditions:** `s` v1 exists; mode READONLY.
- **Steps:** `GET /subjects`, `GET /subjects/{s}/versions/1`.
- **Expected:** both `200 OK` (reads never blocked by READONLY).

---

## 6. Deletion (soft & hard)

> Soft-delete hides items but keeps them; hard-delete (`?permanent=true`) requires a
> prior soft-delete. Hard-delete is blocked while references exist (42206).

| ID | Title | Pri |
|---|---|---|
| KORA-DEL-001 | Soft-delete a subject hides it from default listing | P1 |
| KORA-DEL-002 | Soft-delete returns the deleted version numbers | P2 |
| KORA-DEL-003 | Soft-deleted subject still visible with `?deleted=true` | P2 |
| KORA-DEL-004 | Delete nonexistent subject → 40401 | P1 |
| KORA-DEL-005 | Hard-delete without prior soft-delete → 40405 | P1 |
| KORA-DEL-006 | Hard-delete after soft-delete removes permanently | P1 |
| KORA-DEL-007 | Soft-delete a single version | P1 |
| KORA-DEL-008 | Soft-delete an already soft-deleted version → 40406 | P2 |
| KORA-DEL-009 | Hard-delete a version not soft-deleted first → 40407 | P1 |
| KORA-DEL-010 | Delete `latest` version | P2 |
| KORA-DEL-011 | Delete version on nonexistent subject → 40401 | P2 |
| KORA-DEL-012 | Hard-delete blocked while schema is referenced → 42206 | P1 |
| KORA-DEL-013 | Re-soft-delete an already soft-deleted subject → 40404 | P2 |

### KORA-DEL-001 — Soft-delete subject
- **Priority:** P1
- **Preconditions:** `s` v1 exists.
- **Steps:** `DELETE {base}/subjects/{s}`, then `GET {base}/subjects`.
- **Expected:** delete `200 OK`; `s` absent from default listing.

### KORA-DEL-002 — Returns version numbers
- **Priority:** P2
- **Preconditions:** `s` has v1, v2.
- **Steps:** `DELETE {base}/subjects/{s}`.
- **Expected:** `200 OK`, body `[1, 2]`.

### KORA-DEL-003 — Visible with deleted=true
- **Priority:** P2
- **Preconditions:** `s` soft-deleted.
- **Steps:** `GET {base}/subjects?deleted=true`.
- **Expected:** `s` present in the list.

### KORA-DEL-004 — Delete nonexistent subject
- **Priority:** P1
- **Steps:** `DELETE {base}/subjects/never-existed`.
- **Expected:** `404`, `error_code: 40401`.

### KORA-DEL-005 — Hard-delete precondition
- **Priority:** P1
- **Preconditions:** `s` active (not soft-deleted).
- **Steps:** `DELETE {base}/subjects/{s}?permanent=true`.
- **Expected:** `404`, `error_code: 40405` (must soft-delete first).

### KORA-DEL-006 — Hard-delete success
- **Priority:** P1
- **Preconditions:** `s` soft-deleted.
- **Steps:** `DELETE {base}/subjects/{s}?permanent=true`.
- **Expected:** `200 OK`. `GET /subjects?deleted=true` no longer lists `s`. Re-registering
  `s` starts a fresh history.

### KORA-DEL-007 — Soft-delete one version
- **Priority:** P1
- **Preconditions:** `s` has v1, v2.
- **Steps:** `DELETE {base}/subjects/{s}/versions/1`.
- **Expected:** `200 OK`, body `1`. `GET .../versions` → `[2]`.

### KORA-DEL-008 — Double soft-delete version
- **Priority:** P2
- **Preconditions:** `s` v1 already soft-deleted.
- **Steps:** `DELETE {base}/subjects/{s}/versions/1` again.
- **Expected:** `404`, `error_code: 40406`.

### KORA-DEL-009 — Hard-delete version precondition
- **Priority:** P1
- **Preconditions:** `s` v1 active.
- **Steps:** `DELETE {base}/subjects/{s}/versions/1?permanent=true`.
- **Expected:** `404`, `error_code: 40407`.

### KORA-DEL-010 — Delete latest
- **Priority:** P2
- **Preconditions:** `s` has v1, v2.
- **Steps:** `DELETE {base}/subjects/{s}/versions/latest`.
- **Expected:** `200 OK`, body `2`. Latest is now v1.

### KORA-DEL-011 — Delete version, unknown subject
- **Priority:** P2
- **Steps:** `DELETE {base}/subjects/ghost/versions/1`.
- **Expected:** `404`, `error_code: 40401`.

### KORA-DEL-012 — Hard-delete blocked by reference
- **Priority:** P1
- **Preconditions:** schema `A` v1 is referenced by schema `B`; `A` v1 soft-deleted.
- **Steps:** `DELETE {base}/subjects/A/versions/1?permanent=true`.
- **Expected:** `422`, `error_code: 42206` (references exist).

### KORA-DEL-013 — Re-soft-delete subject
- **Priority:** P2
- **Preconditions:** `s` already soft-deleted.
- **Steps:** `DELETE {base}/subjects/{s}` (without `permanent`).
- **Expected:** `404`, `error_code: 40404` (already soft-deleted; use permanent=true).

---

## 7. Cross-references

> Schemas can reference other registered schemas (`references: [{name, subject, version}]`).
> Used by Protobuf imports, Avro named-type reuse, JSON Schema `$ref`.

| ID | Title | Pri |
|---|---|---|
| KORA-REF-001 | Register a schema with a valid reference | P1 |
| KORA-REF-002 | Register with a reference to a nonexistent subject/version (42201) | P1 |
| KORA-REF-003 | Response echoes `references` when present | P2 |
| KORA-REF-004 | Response omits `references` when empty | P2 |
| KORA-REF-005 | `referencedby` lists referencing schema ids | P1 |
| KORA-REF-006 | `GET /schemas/ids/{id}/versions` lists subject-version pairs | P2 |
| KORA-REF-007 | Referenced version is protected from hard-delete (42206) | P1 |
| KORA-REF-008 | `referencedby` on unknown subject/version → 40401/40402 | P2 |

### KORA-REF-001 — Register with valid reference
- **Priority:** P1
- **Preconditions:** subject `base-schema` v1 exists (a record `B` defining a named type).
- **Steps:** Register a schema under `s` that references `base-schema` v1 via
  `references: [{"name":"B","subject":"base-schema","version":1}]`.
- **Expected:** `200 OK`, returns id + version, `references` echoed.

### KORA-REF-002 — Reference not found
- **Priority:** P1
- **Steps:** Register with `references: [{"name":"X","subject":"missing","version":1}]`.
- **Expected:** `422`, `error_code: 42201` (reference resolution fails before any write).

### KORA-REF-003 — References echoed
- **Priority:** P2
- **Steps:** As KORA-REF-001, inspect response.
- **Expected:** `references` array present and matches input.

### KORA-REF-004 — References omitted when empty
- **Priority:** P2
- **Steps:** Register a schema with no references.
- **Expected:** response JSON has **no** `references` key.

### KORA-REF-005 — referencedby
- **Priority:** P1
- **Preconditions:** `s` references `base-schema` v1.
- **Steps:** `GET {base}/subjects/base-schema/versions/1/referencedby`.
- **Expected:** `200 OK`, array containing the global id of `s`'s schema.

### KORA-REF-006 — versions by schema id
- **Priority:** P2
- **Steps:** `GET {base}/schemas/ids/{id}/versions`.
- **Expected:** `200 OK`, array of `{subject, version}` pairs using that content id.

### KORA-REF-007 — Referenced version protected from hard-delete
- **Priority:** P1
- **Preconditions:** `base-schema` v1 referenced by `s`; `base-schema` v1 soft-deleted.
- **Steps:** `DELETE {base}/subjects/base-schema/versions/1?permanent=true`.
- **Expected:** `422`, `error_code: 42206`.

### KORA-REF-008 — referencedby on unknown target
- **Priority:** P2
- **Steps:** `GET {base}/subjects/ghost/versions/1/referencedby`; then a real subject, unknown version.
- **Expected:** `40401` for unknown subject, `40402` for unknown version (empty-result path).

---

## 8. Confluent wire-compatibility

> These cases protect the core product promise: existing Confluent clients/serializers
> work unmodified. They focus on response *envelope* fidelity, not business logic.

| ID | Title | Pri |
|---|---|---|
| KORA-WIRE-001 | Root `GET /` returns `{}` | P2 |
| KORA-WIRE-002 | All API responses use the schemaregistry content-type | P1 |
| KORA-WIRE-003 | `GET /schemas/types` lists supported types | P2 |
| KORA-WIRE-004 | Error bodies match `{error_code, message}` shape | P1 |
| KORA-WIRE-005 | Boolean query params accept Confluent forms (`true`/`false`) | P2 |
| KORA-WIRE-006 | Content negotiation accepts vnd.schemaregistry Accept header | P2 |
| KORA-WIRE-007 | A real Confluent serializer round-trips against Kora | P1 |

### KORA-WIRE-001 — Root endpoint
- **Priority:** P2
- **Steps:** `GET {base}/`.
- **Expected:** `200 OK`, body `{}`.

### KORA-WIRE-002 — Content-type header
- **Priority:** P1
- **Steps:** Any `GET`/`POST` to an API route; inspect `Content-Type` response header.
- **Expected:** `application/vnd.schemaregistry.v1+json`. (Note: `/metrics` is exempt — see OPS.)

### KORA-WIRE-003 — Supported types
- **Priority:** P2
- **Steps:** `GET {base}/schemas/types`.
- **Expected:** `200 OK`, array containing `AVRO`, `JSON`, `PROTOBUF`.

### KORA-WIRE-004 — Error body shape
- **Priority:** P1
- **Steps:** Trigger several distinct errors (40401, 42201, 40901…).
- **Expected:** every error body has exactly `error_code` (int) and `message` (string),
  with the correct HTTP status (see reference table).

### KORA-WIRE-005 — Boolean param normalization
- **Priority:** P2
- **Steps:** `GET {base}/subjects?deleted=true` and `?deleted=false`.
- **Expected:** parsed correctly (middleware normalizes bool query params).

### KORA-WIRE-006 — Accept header negotiation
- **Priority:** P2
- **Steps:** Send `Accept: application/vnd.schemaregistry.v1+json` and a plain `application/json`.
- **Expected:** both accepted; response content-type is the schemaregistry type.

### KORA-WIRE-007 — Real serializer round-trip (integration)
- **Priority:** P1
- **Preconditions:** a Confluent Avro serializer/deserializer (e.g. `confluent-kafka` client)
  configured with `schema.registry.url = {base}`.
- **Steps:** Serialize a record (auto-registers schema in Kora) → deserialize it back.
- **Expected:** round-trip succeeds with no client modification. **This is the highest-value
  proof of the "drop-in" claim and the closest analogue to the Popsink integration flow.**

---

## 9. Robustness / error handling

| ID | Title | Pri |
|---|---|---|
| KORA-ERR-001 | Malformed JSON body rejected (42201) | P1 |
| KORA-ERR-002 | Missing required `schema` field rejected | P1 |
| KORA-ERR-003 | Body exceeding MAX_BODY_SIZE rejected (413) | P2 |
| KORA-ERR-004 | Empty schema string rejected (42201) | P2 |
| KORA-ERR-005 | Subject name at max length (255) accepted; over → 42201 | P3 |
| KORA-ERR-006 | Subject name with NUL byte rejected (42201) | P3 |
| KORA-ERR-007 | Unknown route returns 404 | P3 |
| KORA-ERR-008 | Wrong HTTP method on a route returns 405 | P3 |
| KORA-ERR-009 | DB unavailable surfaces 50001, not a panic | P2 |
| KORA-ERR-010 | Very large but valid schema is accepted | P3 |

### KORA-ERR-001 — Malformed JSON
- **Priority:** P1
- **Steps:** `POST /subjects/{s}/versions` with body `{ not json`.
- **Expected:** `422`, `error_code: 42201`.

### KORA-ERR-002 — Missing `schema` field
- **Priority:** P1
- **Steps:** `POST /subjects/{s}/versions` with body `{}`.
- **Expected:** `422`, `error_code: 42201`.

### KORA-ERR-003 — Body too large
- **Priority:** P2
- **Preconditions:** default `MAX_BODY_SIZE` (16 MiB).
- **Steps:** POST a body larger than the limit.
- **Expected:** `413 Payload Too Large` (DefaultBodyLimit layer).

### KORA-ERR-004 — Empty schema string
- **Priority:** P2
- **Steps:** `POST .../versions` with `{"schema": ""}`.
- **Expected:** `422`, `error_code: 42201`.

### KORA-ERR-005 — Subject length boundary
- **Priority:** P3
- **Steps:** Register with a 255-char subject (expect OK), then 256-char (expect reject).
- **Expected:** 255 → `200`; 256 → `422`, `42201` ("exceeds maximum length").

### KORA-ERR-006 — NUL byte in subject
- **Priority:** P3
- **Steps:** Register with a subject containing `\0`.
- **Expected:** `422`, `error_code: 42201` ("contains invalid characters").

### KORA-ERR-007 — Unknown route
- **Priority:** P3
- **Steps:** `GET {base}/nope`.
- **Expected:** `404` (router fallback).

### KORA-ERR-008 — Wrong method
- **Priority:** P3
- **Steps:** `DELETE {base}/schemas/types`.
- **Expected:** `405 Method Not Allowed`.

### KORA-ERR-009 — DB unavailable
- **Priority:** P2
- **Preconditions:** PostgreSQL stopped (or pool exhausted).
- **Steps:** Any DB-backed request.
- **Expected:** `500`, `error_code: 50001`; server logs an error but does not crash.

### KORA-ERR-010 — Large valid schema
- **Priority:** P3
- **Steps:** Register a valid schema with many fields, under MAX_BODY_SIZE.
- **Expected:** `200 OK`.

---

## Listing & discovery — `LST`

| ID | Title | Pri |
|---|---|---|
| KORA-LST-001 | `GET /subjects` lists active subjects | P1 |
| KORA-LST-002 | `GET /subjects?deleted=true` includes soft-deleted | P2 |
| KORA-LST-003 | `GET /subjects?deletedOnly=true` returns only soft-deleted | P3 |
| KORA-LST-004 | `subjectPrefix` filter narrows results | P3 |
| KORA-LST-005 | `offset`/`limit` pagination works | P3 |
| KORA-LST-006 | `GET /subjects/{s}/versions` lists versions ascending | P1 |
| KORA-LST-007 | `GET /schemas` lists all registered schemas | P2 |

(Each follows the same template: precondition state → list call → assert exact membership
and ordering. Pagination cases assert `offset`/`limit` slicing and that `limit=-1` means
unlimited.)

---

## Health & metrics — `OPS`

| ID | Title | Pri |
|---|---|---|
| KORA-OPS-001 | `GET /health` returns `{"status":"UP"}` | P1 |
| KORA-OPS-002 | `/health` reflects DB connectivity | P2 |
| KORA-OPS-003 | `GET /metrics` returns Prometheus exposition format | P2 |
| KORA-OPS-004 | `http_requests_total` increments per request | P3 |
| KORA-OPS-005 | `/metrics` is NOT wrapped in the schemaregistry content-type | P3 |

---

## Notes & open questions for the team

1. **Isolation strategy** — current automated tests share one Postgres database and rely
   on UUID-unique subjects/schemas. Any new e2e case author must follow this convention,
   or we move to a database-per-test model. **Decision needed** before scaling the suite.
2. **KORA-WIRE-007** (real Confluent serializer round-trip) and a **Popsink integration
   smoke** are intentionally listed but out of the automation scope of this first pass.
   They are the bridge between "Kora is correct in isolation" and "Kora works in the
   Popsink data flow" — recommended as a fast-follow.
3. **`READONLY_OVERRIDE` / `FORWARD` modes** are accepted as valid and permit writes; their
   nuanced semantics vs. Confluent should be confirmed against the spec (low priority).
4. **Re-registration after delete** (KORA-REG-012) — exact version-numbering behaviour
   after soft- then hard-delete needs to be pinned down by observation and documented here.
</content>
</invoke>
