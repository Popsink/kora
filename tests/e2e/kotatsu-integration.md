# Kora ↔ Kotatsu — End-to-End Integration Test Catalogue

> **Status:** Living document — the primary e2e deliverable. Functional spec of the
> *integration* behaviour, executable by hand (curl + browser) and transposable
> 1-for-1 into Playwright API/UI calls later. **No test code required.**
>
> **Scope:** what happens when **Kotatsu** (the read-only S3 event browser) uses
> **Kora** (the schema registry) to decode Avro events end-to-end. This is the layer
> the developer's unit tests do *not* cover: the real chain, observed from the outside.
>
> **Why this exists (and why it is not `test-cases.md`):** re-testing Kora's REST API
> in isolation duplicates the developer's unit/integration tests. The unique QA value
> is proving Kora does its job *inside the live chain* — a user opens Kotatsu and an
> event is either **readable JSON** (Kora delivered the right schema) or **hex + an
> error** (Kora failed). That is observable, black-box, and ours to own.
> `test-cases.md` and `first-batch.md` remain as the **Kora-in-isolation API reference**.

---

## 1. The chain under test

```
Postgres (source)  ──CDC──▶  Tansu (Kafka-compatible broker, writes to S3)
                                       │
                                       ▼  raw Confluent-Avro bytes in S3
                              Kotatsu (reads S3 directly, on demand)
                                       │  reads schema id from the event…
                                       ▼  …GET /schemas/ids/{id}
                                     Kora  ──▶  returns the Avro schema ("the mould")
                                       │
                                       ▼
                              Kotatsu decodes ──▶ readable JSON in the UI
```

A Confluent-framed Avro event on the wire is: `[0x00][4-byte big-endian schema id][Avro body]`.
Kotatsu reads the id, asks Kora for the matching schema, and decodes the body.
**If Kora is unreachable or returns the wrong/absent schema, Kotatsu cannot decode** —
it falls back to hex and surfaces an error. That fork is the heart of every case below.

---

## 2. Environment & access

| Thing | Value |
|---|---|
| Kotatsu UI | `https://kotatsu.ppsk.localhost/` (self-signed TLS → `curl -k`) |
| Kotatsu API base | `https://kotatsu.ppsk.localhost/api` |
| Kora (the one actually used) | `http://kora.kafka.svc.cluster.local:8080` — **in the Kind cluster**, brought up by the data-plane `inv up`. |
| Cluster name in Kotatsu | `tansu` |

> ⚠️ **Do _not_ run `just dev` in the kora repo for this suite.** That spins up a
> *separate, local* Kora + Postgres that Kotatsu never talks to, and it fights the
> data-plane for port 5432. The Kora under test is the cluster one, reached only
> through Kotatsu.

**Kotatsu API surface used here:**

| Endpoint | Purpose |
|---|---|
| `GET /api/health` | liveness |
| `GET /api/clusters` | list clusters |
| `GET /api/clusters/{c}/topics` | list topics + message counts |
| `GET /api/clusters/{c}/topics/{t}` | partition watermarks (no payloads) |
| `GET /api/clusters/{c}/topics/{t}/messages?partition=&offset=&limit=` | **decoded events** |
| `GET /api/schemas` | registry URL + subject list |
| `GET /api/schemas/{subject}` | one subject's schema, version, id |

**Matching UI routes** (for the browser side of each case): `/` (overview),
`/topics`, `/topics/{name}`, `/schemas`, `/schemas/{subject}`, `/groups`.

> **UI behaviour to remember:** the topic page loads events **on demand** — you must
> pick a partition / `From` (earliest|latest) and click **Search**. No polling.

**Copy-paste setup for the curl steps:**

```bash
export K=https://kotatsu.ppsk.localhost/api
export CL=tansu
# A healthy data topic with Avro events (table `items`, 25 events, schema id 1):
export TOPIC=dfa38c487e7.eecc277a729.c93b93b71d2.public.items
# jq is optional but makes assertions readable.
```

---

## 3. The central assertion — `value.kind`

Every decoded event's `value` (and `key`) carries a `kind` field. **This is the single
most important signal in the whole suite.** Values are taken directly from Kotatsu's
decoder (`backend/src/schema.rs`):

| `value.kind` | Meaning | Verdict on Kora |
|---|---|---|
| `avro` + `data` is JSON, **no `error`** | schema fetched, body decoded | ✅ **Kora OK** |
| `hex` + `schemaId` + `error` | schema could **not** be fetched (Kora down / id absent) | ❌ **Kora failed** |
| `avro` + `data` is hex + `error:"avro decode failed…"` | schema fetched but body didn't match it | ⚠️ schema/data mismatch |
| `raw` | bytes had no `0x00` Confluent frame | n/a (not Avro-framed) |
| `json` / `utf8` | non-Avro field (e.g. the Debezium JSON key) | n/a |

A passing nominal event therefore satisfies: `value.kind == "avro"` **and** `value.error`
is absent **and** `value.data` is a JSON object with the expected fields.

---

## 4. Reference test data (as observed in the live cluster)

- **Subjects in Kora:** `…c68a146751d.public.items-value`, `…c93b93b71d2.public.items-value`,
  `…cc0b4723181.public.items-value` (Confluent `<topic>-value` naming).
- **Topics with Avro data:** `…c93b93b71d2.public.items` (25 events), `…cc0b4723181.public.items` (30 events).
- **`items` record fields:** `id` (long), `item` (string), `qty` (int), `category`
  (nullable string), `in_stock` (boolean), `updated_at` (nullable ZonedTimestamp).
  Wrapped in a Debezium `Envelope` (`before`, `after`, `source`, `op`, `ts_ms`, …).
- **Sample decoded values:** `{id:3,item:"ruler",qty:17,category:"stationery",in_stock:true}`,
  `{id:4,item:"scissors",…}`, `{id:5,item:"glue",…}`.

---

## How to read a case

`KKI-<DOMAIN>-<NNN>` — stable id, never reused. Domains: `DEC` decode nominal ·
`EVO` schema evolution · `RES` resilience / Kora outage · `SCH` Schemas tab.
Priority: `P1` core product promise · `P2` important · `P3` edge.

---

## 5. Scenario 1 — Nominal decode (`DEC`)

> **Proves:** in the live chain, Kotatsu asks Kora for the right schema and renders the
> event as readable JSON. This is *the* reason the integration matters.

### KKI-DEC-001 — An Avro event is shown as readable JSON — `P1`
- **Preconditions:** stack up; `$TOPIC` has ≥1 event; Kora reachable.
- **Steps (API):**
  ```bash
  curl -sk "$K/clusters/$CL/topics/$TOPIC/messages?partition=0&offset=0&limit=5" \
    | jq '.records[0].value'
  ```
- **Steps (UI):** open `/topics/$TOPIC` → `From=earliest` → **Search** → expand the first event.
- **Expected:** `value.kind == "avro"`, **no** `value.error`, and `value.data` is a JSON
  object containing `after.item` (e.g. `"ruler"`), `after.qty`, `after.id`. In the UI the
  row reads as plain JSON, **not** hex.

### KKI-DEC-002 — Event's schema id resolves to a real Kora subject — `P1`
- **Preconditions:** KKI-DEC-001 passed.
- **Steps:** note `value.schemaId` from KKI-DEC-001, then
  ```bash
  curl -sk "$K/schemas" | jq '.subjects'
  ```
- **Expected:** the topic's `…items-value` subject is present in Kora, and the
  `schemaId` Kotatsu used is a valid id for it. (Closes the loop: the id in the event
  is the id Kora knows.)

### KKI-DEC-003 — Every event on a healthy topic decodes — `P2`
- **Steps:**
  ```bash
  curl -sk "$K/clusters/$CL/topics/$TOPIC/messages?partition=0&offset=0&limit=100" \
    | jq '[.records[].value.kind] | group_by(.) | map({(.[0]): length}) | add'
  ```
- **Expected:** `{"avro": 25}` — i.e. **zero** `hex` and zero `value.error` across the
  whole partition. A single `hex` here is a finding.

### KKI-DEC-004 — Non-Avro key is rendered without error — `P2`
- **Steps:** inspect `.records[0].key` from the same fetch.
- **Expected:** the Debezium key is shown as `kind:"utf8"`/`"json"` (it is JSON, not
  Confluent-Avro) — Kotatsu must **not** force-Avro it or hex it. Confirms field-level
  format detection is independent per field.

### KKI-DEC-005 — Logical/typed fields stay human-readable — `P3`
- **Steps:** inspect `after.in_stock`, `after.updated_at`, `after.qty` in a decoded event.
- **Expected:** `in_stock` is a real boolean, `qty` an integer, `updated_at` a readable
  timestamp string (not hex bytes) — i.e. Kora's logical-type schema is honoured by the
  decoder.

---

## 6. Scenario 2 — Schema evolution (`EVO`)

> **Proves:** when the source schema changes, Kora serves multiple versions and Kotatsu
> resolves **each event by its own schema id** — so new events become readable **without
> breaking the old ones**.

### KKI-EVO-001 — New events after an evolution decode against the new version — `P1`
- **Preconditions:** ability to evolve the source table (e.g. `ALTER TABLE items ADD
  COLUMN sku text`) and let CDC produce new events; **backward-compatible** change.
- **Steps:** evolve + insert a row → in Kotatsu, **Search** `From=latest` on `$TOPIC`.
- **Expected:** the newest event has `value.kind == "avro"`, no `error`, and `value.data`
  includes the new field (`after.sku`). Its `schemaId` differs from the v1 events'.

### KKI-EVO-002 — Pre-evolution events remain readable — `P1`
- **Preconditions:** KKI-EVO-001 done (registry now holds ≥2 versions of the subject).
- **Steps:** **Search** `From=earliest` and inspect the *oldest* events.
- **Expected:** old events still `kind:"avro"`, no `error`, decoded with their **original**
  `schemaId` (the new field simply absent). **No regression** — this is the core promise
  of schema evolution and the most valuable assertion of the scenario.

### KKI-EVO-003 — Both versions visible in the Schemas tab — `P2`
- **Steps:** `curl -sk "$K/schemas/<subject>" | jq '{versions, latest: (.latest | {id, version, schemaType})}'` and the
  `/schemas/{subject}` UI page.
- **Expected:** the subject reports the latest version (≥2) while older events still map
  to earlier ids; type stays `AVRO`.

---

## 7. Scenario 3 — Resilience when Kora is down (`RES`)

> **Proves:** a translator outage degrades gracefully — Kotatsu stays up, shows hex + a
> clear error, and recovers when Kora returns. A Kora failure must never crash the browser.

> **Precondition (blocker resolved 2026-06-11):** these need Kora scaled down in the
> `kafka` namespace. No local `kubectl` exists; go **through the Kind node container**,
> which ships its own kubectl + admin kubeconfig:
>
> ```bash
> # scale down / up
> docker exec -i popsink-data-plane-dev-control-plane \
>   kubectl --kubeconfig /etc/kubernetes/admin.conf -n kafka scale deploy/kora --replicas=0
> docker exec -i popsink-data-plane-dev-control-plane \
>   kubectl --kubeconfig /etc/kubernetes/admin.conf -n kafka scale deploy/kora --replicas=1
> ```

### KKI-RES-001 — With Kora down, events show hex + error, UI survives — `P1`
- **Steps:** scale Kora to 0 → in Kotatsu **Search** `$TOPIC` (use a fresh fetch; cached
  schemas may still decode, so prefer a topic/id not yet fetched this session).
- **Expected:** affected events become `value.kind == "hex"` with a `schemaId` and an
  `error`; the page still returns HTTP 200; offsets, headers, key and topic list still
  render. **No crash, no blank page.**

### KKI-RES-002 — Schemas tab degrades cleanly when Kora is down — `P1`
- **Steps:** with Kora at 0 replicas, open `/schemas` (and `GET /api/schemas`).
- **Expected:** a clear error/empty state surfaced to the user — **not** a stack trace,
  hang, or 500 that takes the page down.

### KKI-RES-003 — Recovery: decode resumes after Kora returns — `P1`
- **Steps:** scale Kora back to 1 → re-**Search** the same topic.
- **Expected:** events decode again (`kind:"avro"`, no `error`). Confirms the failure was
  transient and Kotatsu re-fetches schemas rather than caching the failure.

### KKI-RES-004 — Unknown/deleted schema id behaves like an outage — `P3`
- **Steps:** target an event whose schema id is absent from Kora (e.g. after a hard-delete
  of that subject in a throwaway environment), with Kora **up**.
- **Expected:** `kind:"hex"`, `schemaId` present, `error` present — same graceful path as
  a full outage; the rest of the event (headers, key, offset) still renders.

---

## 8. Scenario 4 — Schemas tab reflects Kora (`SCH`)

> **Proves:** Kotatsu's Schemas view is a faithful, read-only mirror of Kora's content.

### KKI-SCH-001 — Schemas tab lists every Kora subject — `P1`
- **Steps:** `curl -sk "$K/schemas" | jq '.subjects'` vs the `/schemas` UI list.
- **Expected:** the three `…public.items-value` subjects appear in both; counts match;
  no subject missing or invented.

### KKI-SCH-002 — Subject detail matches Kora — `P1`
- **Steps:** open `/schemas/<subject>` (the UI page you saw: type, latest version, schema
  id, full Avro schema).
- **Expected:** `type = AVRO`, a version (e.g. `1`), a `schema id` (e.g. `1`), and the full
  `Envelope` schema with the `items` fields — all consistent with what Kora returns.

### KKI-SCH-003 — Displayed registry URL matches the configured Kora — `P2`
- **Steps:** `curl -sk "$K/schemas" | jq '.registry'`.
- **Expected:** `http://kora.kafka.svc.cluster.local:8080` — proves Kotatsu is wired to the
  cluster Kora (not a stray local one).

### KKI-SCH-004 — Unknown subject errors cleanly — `P3`
- **Steps:** `curl -sk -o /dev/null -w "%{http_code}\n" "$K/schemas/does-not-exist-xyz"`.
- **Expected:** a clean 404/error response and a graceful UI state — no crash.

---

## 9. Execution checklist

| Case | Pri | Proves | Pass? |
|---|:--:|---|:--:|
| KKI-DEC-001 | P1 | Avro event → readable JSON | ✅ 2026-06-11 |
| KKI-DEC-002 | P1 | Event id resolves to a real subject | ✅ 2026-06-11 |
| KKI-DEC-003 | P2 | Whole partition decodes (0 hex) | ✅ 2026-06-11 |
| KKI-DEC-004 | P2 | Non-Avro key not force-decoded | ✅ 2026-06-11 |
| KKI-DEC-005 | P3 | Logical types stay readable | ✅ 2026-06-11 |
| KKI-EVO-001 | P1 | New events use the new version | ☐ |
| KKI-EVO-002 | P1 | Old events still readable (no regression) | ☐ |
| KKI-EVO-003 | P2 | Both versions visible | ☐ |
| KKI-RES-001 | P1 | Kora down → hex + error, UI alive | ⚠️ FAIL partial 2026-06-11 (ANO-1, ANO-2) |
| KKI-RES-002 | P1 | Schemas tab degrades cleanly | ✅ 2026-06-11 |
| KKI-RES-003 | P1 | Decode resumes after recovery | ✅ 2026-06-11 |
| KKI-RES-004 | P3 | Unknown id behaves like outage | ⏭ skipped (needs subject hard-delete; reference data irreplaceable until a source pipeline is re-provisioned) |
| KKI-SCH-001 | P1 | Schemas list mirrors Kora | ✅ 2026-06-11 |
| KKI-SCH-002 | P1 | Subject detail matches Kora | ✅ 2026-06-11 |
| KKI-SCH-003 | P2 | Registry URL is the cluster Kora | ✅ 2026-06-11 |
| KKI-SCH-004 | P3 | Unknown subject errors cleanly | ✅ 2026-06-11 |

---

## 10. Notes & open items

- **Playwright transposition:** each curl maps to a `request.get(...)` asserting on
  `response.status()` and the parsed body; each UI step maps to `page.goto` +
  `getByRole('button',{name:/search/i}).click()` + assertions on the rendered rows. The
  `value.kind` checks become the primary `expect(...)`.
- **Blocker — scenario 3: resolved** (2026-06-11): use the Kind node's embedded kubectl
  via `docker exec` (see scenario 3 precondition).
- **Blocker — scenario 2 (EVO):** the source Postgres that produced the reference events
  (db `popsink_source`, connector `e2e-source-pg`) **no longer exists** — it is not on the
  cluster's `postgresql-0` (only `data_plane`, `control_plane`, `kora`… databases) and no
  other container is present. No CDC pipeline is currently running (no `workers` pods);
  the reference events are S3 leftovers from a previous session. EVO needs either a
  re-provisioned source + pipeline (full chain) or synthetic v2 Avro events produced
  straight to Tansu (strict Kora↔Kotatsu scope).
- **Findings — KKI-RES-001 (2026-06-11), verdict FAIL partial.** Structural resilience
  holds (HTTP 200, hex fallback, keys/offsets intact, no crash, clean recovery), but:
  - **ANO-1 — hex fallback carries no diagnostic** (filed as [data-plane#2650](https://github.com/Popsink/data-plane/issues/2650)). With Kora down, affected events
    come back as `{kind:"hex", data}` — **no `schemaId`, no `error`**. Root cause: the
    deployed image is `ghcr.io/popsink/kotatsu:0.1.0`, whose `decode_field` silently
    falls through to `raw_field` on registry failure. The diagnostic path
    (`{kind:"hex", schemaId, error:"schema id N: …"}`) exists since v0.2.0
    (commit `8aedf61`). **Recommendation:** bump the kotatsu image in the data-plane
    deployment to ≥0.2.0, then re-run this case.
  - **ANO-2 — registry client has no timeout** (filed as [kotatsu#55](https://github.com/Popsink/kotatsu/issues/55)). `SchemaRegistry` uses
    `reqwest::Client::new()` (no timeout, no negative caching) — `schema.rs:77`, present
    in 0.1.0 **and** current HEAD. With Kora's Service blackholed, message fetches
    intermittently hang **~25-30 s** (UI stuck on "Loading…", no feedback); other runs
    fail instantly (502 in 0.17 s on `/api/schemas`). **Recommendation:** set a short
    client timeout (1-2 s) and consider briefly caching fetch failures.
- **Minor observation (KKI-SCH-004 + KKI-RES-002, 2026-06-11):** error messages leak
  internal registry routes/URLs (404: `subject '/subjects/<name>/versions' not found`;
  502: full in-cluster Kora URL). Cosmetic; filed as
  [kotatsu#56](https://github.com/Popsink/kotatsu/issues/56).
- **Caching caveat (scenario 3):** Kotatsu caches resolved schemas (ids are immutable),
  so an already-fetched id may still decode after Kora goes down. Use a not-yet-fetched
  topic/id, or a fresh session, to observe the outage path honestly.
- **P2/P3** cases can be promoted into a focused execution batch once the P1 path is green,
  the same way `first-batch.md` was carved out of `test-cases.md`.
</content>
</invoke>
