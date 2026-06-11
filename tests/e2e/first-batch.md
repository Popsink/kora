# Kora — First batch: manual P1 runbook

> **Goal:** a ready-to-run checklist of the most critical (P1) end-to-end cases from
> [`test-cases.md`](./test-cases.md). Each step is a copy-paste `curl` command plus the
> result you should see. Run it by hand, or transpose each request 1-for-1 into a
> Playwright API request later. **No code required.**
>
> Covers the 3 pillars of Kora: **register & version**, **read back**, **reject the
> dangerous change**, plus **delete safely**.

## Before you start

1. Start Kora locally (this also starts PostgreSQL):
   ```bash
   just dev
   ```
2. Set two shell variables so every command below is copy-paste ready. The unique
   subject avoids collisions with previous runs (Kora dedups schemas globally):
   ```bash
   export BASE=http://localhost:8080
   export SUBJECT=clients-$(date +%s)
   echo "Using subject: $SUBJECT"
   ```
3. Sanity check — the registry is up:
   ```bash
   curl -s $BASE/health
   # expected: {"status":"UP"}
   ```

**Schemas used in this runbook (a "Client" record):**

| Name | Schema | Note |
|---|---|---|
| V1 | `{"type":"record","name":"Client","fields":[{"name":"id","type":"int"}]}` | just an id |
| V2 | `…,{"name":"name","type":["null","string"],"default":null}` | adds **optional** name → safe |
| INCOMPAT | `…,{"name":"email","type":"string"}` | adds **required** email → dangerous |

---

## Results checklist

Tick as you go. Expected HTTP status and key body content are in each section.

| Case | What it proves | Pass? |
|---|---|:---:|
| KORA-REG-001 | Register a new schema → version 1 | ☐ |
| KORA-REG-002 | Re-register same schema is idempotent | ☐ |
| KORA-REG-003 | Safe evolution → version 2 | ☐ |
| KORA-GET-003 | Read schema by version | ☐ |
| KORA-GET-004 | Read `latest` | ☐ |
| KORA-GET-005 | Unknown subject → 40401 | ☐ |
| KORA-GET-006 | Unknown version → 40402 | ☐ |
| KORA-CMP-002 | Incompatible change detected | ☐ |
| KORA-CMP-003 | Registration of incompatible schema blocked → 40901 | ☐ |
| KORA-MOD-002 | READONLY mode blocks writes → 42205 | ☐ |
| KORA-DEL-001 | Soft-delete hides the subject | ☐ |
| KORA-DEL-005 | Hard-delete without soft-delete first → 40405 | ☐ |
| KORA-DEL-006 | Hard-delete after soft-delete succeeds | ☐ |

---

## Pillar 1 — Register & version

### KORA-REG-001 — Register a new schema
```bash
curl -s -X POST $BASE/subjects/$SUBJECT/versions \
  -H "Content-Type: application/vnd.schemaregistry.v1+json" \
  -d '{"schema":"{\"type\":\"record\",\"name\":\"Client\",\"fields\":[{\"name\":\"id\",\"type\":\"int\"}]}"}'
```
**Expected:** `200`, body like `{"id":<N>,"version":1,"schemaType":"AVRO","schema":"..."}`.
→ Note the `id` (call it `ID1`); `version` must be `1`.

### KORA-REG-002 — Re-register the exact same schema (idempotent)
Run **the same command as KORA-REG-001 again**.
**Expected:** `200`, **same `id` as `ID1`**, **`version:1`** (no new version created).

### KORA-REG-003 — Safe evolution adds version 2
```bash
curl -s -X POST $BASE/subjects/$SUBJECT/versions \
  -H "Content-Type: application/vnd.schemaregistry.v1+json" \
  -d '{"schema":"{\"type\":\"record\",\"name\":\"Client\",\"fields\":[{\"name\":\"id\",\"type\":\"int\"},{\"name\":\"name\",\"type\":[\"null\",\"string\"],\"default\":null}]}"}'
```
**Expected:** `200`, `version:2`, a new `id`. The optional `name` keeps old readers safe.
Confirm the history:
```bash
curl -s $BASE/subjects/$SUBJECT/versions
# expected: [1,2]
```

---

## Pillar 2 — Read back

### KORA-GET-003 — Read schema by version
```bash
curl -s $BASE/subjects/$SUBJECT/versions/1
```
**Expected:** `200`, body contains `subject`, `version:1`, `id`, `schema`.

### KORA-GET-004 — Read `latest`
```bash
curl -s $BASE/subjects/$SUBJECT/versions/latest
```
**Expected:** `200`, `version:2` (latest after the evolution).

### KORA-GET-005 — Unknown subject
```bash
curl -s -o /dev/null -w "%{http_code}\n" $BASE/subjects/does-not-exist-xyz/versions/1
curl -s $BASE/subjects/does-not-exist-xyz/versions/1
```
**Expected:** HTTP `404`, body `{"error_code":40401,"message":"Subject not found"}`.

### KORA-GET-006 — Unknown version on an existing subject
```bash
curl -s -o /dev/null -w "%{http_code}\n" $BASE/subjects/$SUBJECT/versions/99
curl -s $BASE/subjects/$SUBJECT/versions/99
```
**Expected:** HTTP `404`, body `error_code:40402` (Version not found).
> Key check: subject exists → `40402`, not `40401`. The two errors must differ.

---

## Pillar 3 — Reject the dangerous change

### KORA-CMP-002 — The incompatible change is detected
Test the dangerous schema (required `email`) against the latest version, *without*
registering it:
```bash
curl -s -X POST $BASE/compatibility/subjects/$SUBJECT/versions/latest \
  -H "Content-Type: application/vnd.schemaregistry.v1+json" \
  -d '{"schema":"{\"type\":\"record\",\"name\":\"Client\",\"fields\":[{\"name\":\"id\",\"type\":\"int\"},{\"name\":\"email\",\"type\":\"string\"}]}"}'
```
**Expected:** `200`, body `{"is_compatible":false}`. (Add `?verbose=true` to see why.)

### KORA-CMP-003 — Registering the incompatible schema is blocked
Now actually try to register that same dangerous schema:
```bash
curl -s -o /dev/null -w "%{http_code}\n" -X POST $BASE/subjects/$SUBJECT/versions \
  -H "Content-Type: application/vnd.schemaregistry.v1+json" \
  -d '{"schema":"{\"type\":\"record\",\"name\":\"Client\",\"fields\":[{\"name\":\"id\",\"type\":\"int\"},{\"name\":\"email\",\"type\":\"string\"}]}"}'
```
**Expected:** HTTP `409`, body `error_code:40901`. Confirm no version 3 was created:
```bash
curl -s $BASE/subjects/$SUBJECT/versions
# expected: still [1,2]
```

### KORA-MOD-002 — READONLY mode blocks writes
```bash
# 1. Put the whole registry in read-only
curl -s -X PUT $BASE/mode -H "Content-Type: application/vnd.schemaregistry.v1+json" -d '{"mode":"READONLY"}'
# 2. Try to register anything
curl -s -o /dev/null -w "%{http_code}\n" -X POST $BASE/subjects/$SUBJECT/versions \
  -H "Content-Type: application/vnd.schemaregistry.v1+json" \
  -d '{"schema":"{\"type\":\"record\",\"name\":\"Client\",\"fields\":[{\"name\":\"id\",\"type\":\"int\"}]}"}'
```
**Expected:** step 2 → HTTP `422`, `error_code:42205` (Operation not permitted).
**Teardown — IMPORTANT, restore writes before continuing:**
```bash
curl -s -X PUT $BASE/mode -H "Content-Type: application/vnd.schemaregistry.v1+json" -d '{"mode":"READWRITE"}'
```

---

## Pillar 4 — Delete safely

### KORA-DEL-001 — Soft-delete hides the subject
```bash
curl -s -X DELETE $BASE/subjects/$SUBJECT          # expected: [1,2]
curl -s $BASE/subjects | tr ',' '\n' | grep $SUBJECT   # expected: no output (hidden)
curl -s "$BASE/subjects?deleted=true" | tr ',' '\n' | grep $SUBJECT  # expected: still listed
```
**Expected:** delete returns the deleted versions `[1,2]`; subject gone from the default
list but visible with `?deleted=true`.

### KORA-DEL-005 — Hard-delete requires a prior soft-delete
Create a fresh, **active** subject and try to permanently delete it directly:
```bash
export SUBJECT2=clients-hard-$(date +%s)
curl -s -X POST $BASE/subjects/$SUBJECT2/versions \
  -H "Content-Type: application/vnd.schemaregistry.v1+json" \
  -d '{"schema":"{\"type\":\"record\",\"name\":\"Other\",\"fields\":[{\"name\":\"x\",\"type\":\"string\"}]}"}'
curl -s -o /dev/null -w "%{http_code}\n" -X DELETE "$BASE/subjects/$SUBJECT2?permanent=true"
curl -s -X DELETE "$BASE/subjects/$SUBJECT2?permanent=true"
```
**Expected:** HTTP `404`, `error_code:40405` (must soft-delete first).

### KORA-DEL-006 — Hard-delete succeeds after soft-delete
```bash
curl -s -X DELETE $BASE/subjects/$SUBJECT2                       # soft-delete first → [1]
curl -s -X DELETE "$BASE/subjects/$SUBJECT2?permanent=true"      # then permanent
curl -s "$BASE/subjects?deleted=true" | tr ',' '\n' | grep $SUBJECT2   # expected: no output
```
**Expected:** soft-delete `200`; permanent delete `200`; subject no longer listed even
with `?deleted=true`.

---

## When you're done

- Stop the local stack: `Ctrl-C` on `just dev` (it tears down PostgreSQL automatically).
- Record any case that did **not** match the expected result — that's a finding to log
  against the corresponding `KORA-*` id in `test-cases.md`.

## Notes

- This batch is **execution-ready** and tool-agnostic. To semi-automate with the
  Playwright CLI, each `curl` maps directly to a `request.post/get/delete(...)` call
  with the same URL, header and JSON body, asserting on `response.status()` and the
  parsed body.
- Lower-priority (P2/P3) cases stay in `test-cases.md` and can be promoted into a
  "second batch" runbook the same way once this one is green.
</content>
