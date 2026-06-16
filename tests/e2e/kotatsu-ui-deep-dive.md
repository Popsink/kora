# Kotatsu UI — Deep-Dive E2E / White-Box Test Session

> **Date:** 2026-06-15 · **Target:** deployed Kotatsu `0.2.1` (image
> `ghcr.io/popsink/kotatsu:0.2.1`), Kora `v0.3.3`, live Kind cluster.
> **Method:** black-box e2e through the real UI (Playwright, headed) + HTTP API probes,
> **grounded in the source at the exact deployed commit** (`~/Documents/kotatsu` @ `b3d558e`,
> tag boundary `v0.2.1`) — i.e. white-box-informed e2e. No app code was modified.
> **Data under test:** topic `…c197b708407.pokemon` (1350 Avro events, schema id 15),
> freshly produced by a DLT source → Postgres subscription. Companion to
> [`kotatsu-integration.md`](./kotatsu-integration.md).

## Scope & approach

The session targets the layer unit tests don't cover: the live UI behaviour of the
Kora↔Kotatsu chain, plus input/error/edge handling. Test ideas were derived from a
white-box read of the frontend (`frontend/pages/topics/[name].vue`, list pages, decoding
render helpers) and backend (`backend/src/api.rs`, `schema.rs`, `storage/reader.rs`), so each
case exercises a real branch rather than a generic checklist item.

## Confirmed in 0.2.1 (regression checks vs the 2026-06-11 findings)

| Prior finding | Status in 0.2.1 | Evidence |
|---|---|---|
| ANO-1 — hex fallback had no `schemaId`/`error` | **Fixed — confirmed live** | `schema.rs:228,245` emit `{kind:"hex", schemaId, error}`; outage test returned `{kind:"hex", schemaId:13, error:"schema id 13: schema registry is unreachable"}` (API + UI). |
| ANO-2 — registry client had no timeout | **Fixed — confirmed live** | `schema.rs:77-91` sets `connect_timeout=2s`, `timeout=5s`; outage responses came back in ~0.01 s with no multi-second hang. |
| kotatsu#56 — error messages leak internal Kora URL | **Partially fixed** | Schema/registry errors now sanitized (`/schemas` down → `{"error":"schema registry is unreachable"}`, no URL), **but storage errors still leak** — see KUI-BUG-1. |

## Executed cases

Legend: ✅ pass · 🐛 defect · 📝 observation · ⏸ blocked.

| ID | Area | Action → Expected | Result |
|---|---|---|---|
| API-1 | validation | `offset=foobar` → 400 + clear msg | ✅ `400 {"error":"invalid offset: foobar"}` |
| API-2 | validation | `value_contains=[`&`regex=true` → 400 | ✅ `400 invalid regex: …` |
| API-3 | bounds | `limit=600` → clamped to 500 | ✅ `count:500` |
| API-4 | bounds | `limit=0` → clamped to 1 | ✅ `count:1` |
| API-5 | error | `partition=999` → clean not-found | 🐛 **KUI-BUG-1** (leaks S3 path) |
| API-6 | state | purged items topic → empty + signal | 🐛 **KUI-BUG-2** (`count:0` but `watermark.high:25`) |
| API-7 | error | non-existent topic → clean not-found | 🐛 **KUI-BUG-1** (leaks S3 path) |
| UI-1 | state | purged topic in UI → clear empty state | 🐛 **KUI-BUG-2** ("25 messages" header + "No messages in this range") |
| UI-2 | a11y | expand a message row via keyboard | 🐛 **KUI-BUG-3** (row not focusable) |
| UI-3 | error | invalid offset in UI → error shown, no crash | ✅ error surfaced, no crash |
| UI-4 | bounds | `Limit=600` → HTML `max=500`, backend clamps | ✅ no crash, ≤500 rendered |
| UI-5 | persistence | value/key format persists across nav | ✅ `localStorage kotatsu:fmt` retained |
| UI-6 | export | Export JSON → file downloads | ✅ `…pokemon-p0.json` |
| UI-7 | filter | `value_contains=charizard` → filtered + flag | ✅ filtered rows + scan indicator |
| UI-8 | groups | lazy-load consumer groups on topic page | ✅ list/state rendered |
| UI-9 | schemas | schema detail page renders | ✅ type AVRO, fields, version selector |
| UI-10 | pagination | topics list shows "x–y of N" | ✅ "1–21 of 21" |
| UI-11 | pagination | rapid Next clicks → no stale results | ✅ Next disabled during fetch |
| UI-12 | export | Export NDJSON → file downloads | ✅ `…pokemon-p0.ndjson` |
| UI-13 | clipboard | Copy JSON → "Copied ✓" feedback | ✅ works (initial fail was a mis-targeted selector — false positive, retracted) |
| UI-14 | perf | `limit=500` → renders without crash | 📝 **KUI-OBS-1** (503 rows, ~2 s, no virtualization) |
| RES-001 | resilience | Kora down, uncached id → hex + `schemaId` + `error` | ✅ `{kind:"hex", schemaId:13, error:"schema id 13: schema registry is unreachable"}` (API + UI) |
| RES-001t | resilience | Kora down → no multi-second hang (0.2.1 timeout) | ✅ ~0.01 s responses, 502 in 0.006 s — no 25-30 s hang |
| RES-002 | resilience | `/schemas` down → clean error, no URL leak | ✅ `502 {"error":"schema registry is unreachable"}`, internal URL not leaked |
| RES-cache | resilience | cached schema id still decodes during outage | ✅ pokemon (id 15) stays `avro` while Kora down |
| RES-003 | resilience | decode resumes after Kora returns | ✅ TOPIC2 back to `avro` (id 13) after scale-up |

> **Methodology note (rigour):** UI-2 and UI-13 first reported false positives because the
> selector `table tbody tr` matched the *partitions* table, not the *messages* table (the
> topic page renders several tables). Re-tested against the message row: row expansion **by
> click works** and Copy JSON **works** (UI-13 retracted); keyboard inaccessibility **is
> real** (UI-2 confirmed). Lesson recorded so the Playwright transposition targets the
> message table explicitly.

---

## Defects (ISTQB format)

### KUI-BUG-1 — Storage error responses leak internal S3 object paths — `P2` · filed as [kotatsu#63](https://github.com/Popsink/kotatsu/issues/63)
- **Description:** `GET …/messages` for an out-of-range partition or a non-existent topic
  returns a 404 whose body exposes the internal S3 layout instead of a user-meaningful
  message.
- **Steps:**
  1. `GET /api/clusters/tansu/topics/<topic>/messages?partition=999&offset=earliest&limit=2`
  2. `GET /api/clusters/tansu/topics/does.not.exist/messages?partition=0&offset=earliest&limit=2`
- **Expected:** a clean, user-facing error, e.g. `"partition 999 out of range (topic has N
  partitions)"` and `"topic 'does.not.exist' not found"`.
- **Actual:** `{"error":"object not found: clusters/tansu/topics/…/partitions/0000000999/watermark.json"}`
  — leaks bucket layout, zero-padded partition encoding, and the `watermark.json` filename.
- **Notes:** the 0.2.1 "error hygiene" pass sanitized *schema-registry* errors (`api.rs:subject_err`)
  but **not** *storage* errors, which still surface `StorageError`'s `to_string()`
  (`storage/error.rs`, "object not found: {key}"). Same class as kotatsu#56, different code path.
  Two problems in one: (a) information disclosure of internal structure; (b) non-actionable
  message for the user/operator. A non-existent topic and an out-of-range partition are also
  indistinguishable.
- **Suggested fix:** map `StorageError::NotFound` to a sanitized API error in `api.rs` (distinguish
  topic-not-found vs partition-out-of-range), and keep the raw key only in server logs.

### KUI-BUG-2 — Phantom message count: purged records still advertise a non-zero count — `P2` · filed as [kotatsu#64](https://github.com/Popsink/kotatsu/issues/64)
- **Description:** when a topic's S3 record batches are purged (retention) but `watermark.json`
  survives, both the API and the UI keep advertising the old message count while serving zero
  messages, with no error or empty-state explanation. (Same root cause as ANO-3 in
  `kotatsu-integration.md`; this entry adds the UI-level confirmation.)
- **Steps (API):** `GET …/topics/<purged-items-topic>/messages?partition=0&offset=earliest&limit=5`
- **Steps (UI):** open `/topics/<purged-items-topic>` → From=earliest → Search.
- **Expected:** the UI signals that records are unavailable/purged (or reconciles the count to
  the readable range), rather than implying data exists.
- **Actual:** API returns `count:0, scanned:0, exhausted:true, watermark:{low:0, high:25}`. The
  UI header reads "partition 0 — low 0, high 25 (25 messages)" while the body shows
  "No messages in this range" — a user sees "25 messages" yet can open none, with no hint why.
- **Notes:** backend has no dedicated flag for "records gone, watermark stale"
  (`storage/reader.rs`: empty base-offset listing returns `[]` while `watermark()` still reads
  the persisted high). The frontend can only infer it from `count==0 && watermark.high>0`
  (`pages/topics/[name].vue:324-326`).
- **Suggested fix:** detect `count==0 && scanned==0 && watermark.high>low` in the backend and
  return an explicit marker (e.g. `records_unavailable: true`), and render a distinct UI state
  ("records for this range are no longer available").

### KUI-BUG-3 — Message rows are not keyboard-accessible — `P2` (a11y)
- **Description:** message rows expand on mouse click only. They carry no `tabindex` and no
  `role`, so keyboard-only and screen-reader users cannot focus a row or open its detail
  (full value, headers, schema link, Copy JSON).
- **Steps:** load events on `/topics/…pokemon` → Tab toward the message table → attempt to focus
  the first message row and press Enter.
- **Expected:** rows are focusable (`tabindex="0"`, `role="button"`/`aria-expanded`) and toggle
  on Enter/Space.
- **Actual:** `tabindex=null`, `role=null`; `document.activeElement` stays on `<body>`; Enter
  does nothing. Detail is reachable by mouse only.
- **Notes:** `pages/topics/[name].vue:341-349` binds `@click` on the `<tr>` with no keyboard
  handler. WCAG 2.1.1 (Keyboard) / 4.1.2 (Name, Role, Value).
- **Suggested fix:** add `tabindex="0"`, `role="button"`, `:aria-expanded`, and a
  `@keydown.enter.space.prevent` handler mirroring the click toggle.

### KUI-BUG-4 — Consumer-groups load failure is rendered as "none" (error disguised as empty) — `P2` · filed as [kotatsu#66](https://github.com/Popsink/kotatsu/issues/66)
- **Description:** on the topic page, the lazy-loaded "Consumer groups" panel shows `none` both
  when a topic genuinely has no consumer groups **and** when the load request fails. A failure is
  silently turned into a reassuring "no groups" state — same "invisible state" class as KUI-BUG-2.
- **Steps (error-injection, Playwright):**
  1. Open `/topics/…c197b708407.pokemon` (which has 1 real consumer group, `52f73d0a…`).
  2. Intercept `**/api/clusters/*/topics/*/groups` and fulfill it with HTTP 500.
  3. Click "Consumer groups".
- **Expected:** the UI surfaces a load error (e.g. "couldn't load consumer groups").
- **Actual:** the group vanishes and the UI shows **`none`** with **no error** — identical to a
  topic that truly has zero groups. Confirmed live: normal load shows `52f73d0a…` + lag/offsets;
  with the request failing, only `none` is shown.
- **Impact:** an operator checking "is anything consuming this topic?" sees `none` and may
  conclude the pipeline is broken (and act on it) when in fact the panel just failed to load —
  a false negative on a monitoring-relevant signal.
- **Notes:** `pages/topics/[name].vue` `loadGroups()` ~L41-52 does `catch { topicGroups.value = [] }`,
  and the template renders `[]` as "none" (~L249) — indistinguishable from a successful empty result.
- **Suggested fix:** keep a distinct error state in the catch branch (e.g. `topicGroups = 'error'`)
  and render "couldn't load consumer groups" instead of "none".

---

## Observations (not defects)

### KUI-OBS-1 — Large result sets are not virtualized — `P3`
`limit=500` renders ~503 rows (plus per-row detail templates) with no virtual scrolling
(`pages/topics/[name].vue` `v-for` over `records`); ~2 s to render and the page grows heavy as
rows are expanded. Acceptable at 500 (the hard cap), but worth a note for UX/perf and a virtual
list if the cap ever rises.

### KUI-OBS-2 — Clipboard copy failures are swallowed silently — `P3` · filed as [kotatsu#65](https://github.com/Popsink/kotatsu/issues/65)
Copy JSON works in normal conditions (secure context + permission → "Copied ✓"). But
`pages/topics/[name].vue:175` does `catch {}` — if the Clipboard API rejects (no permission,
insecure context, oversized payload) the user gets no feedback at all. Consider surfacing a
"copy failed" hint.

---

## Resilience scenario — executed 2026-06-15 (all PASS)

Kora was scaled to 0 replicas (with explicit user go-ahead), the outage path probed against an
**uncached** schema id (`…E2E_TESTING_SOURCE_E2E_TOPIC2`, id 13), then Kora scaled back to 1:

- **RES-001** — uncached id with Kora down → `{kind:"hex", schemaId:13, error:"schema id 13:
  schema registry is unreachable"}`; the UI shows the hex payload + the "unreachable" warning and
  stays alive (no blank page). **The 2026-06-11 ANO-1 FAIL is resolved in 0.2.1.**
- **RES-001 (timing)** — responses returned in ~0.01 s; `/schemas` 502 in 0.006 s. **No 25-30 s
  hang (ANO-2 resolved).** (Connection is refused fast since the Service has no endpoints; the
  5 s timeout would also bound a true blackhole.)
- **RES-002** — `/schemas` with Kora down → `502 {"error":"schema registry is unreachable"}`,
  internal Kora URL **not** leaked.
- **RES-cache** — pokemon (id 15, already resolved) kept decoding as `avro` during the outage —
  confirms immutable-id schema caching.
- **RES-003** — after scaling Kora back to 1, TOPIC2 decoded again as `avro` (id 13). Clean recovery.

The environment was restored: Kora back at 1 replica, endpoints routing, `/schemas` returns its
11 subjects.

## Reproduction

Scripts under `~/.kora-e2e-scratch/`: `deep-ui.mjs` (UI-1…7), `deep-ui2.mjs` (UI-8…14),
`diag-expand2.mjs` (row-targeting fix for UI-2/UI-13). Screenshots `ui1-*`, `ui7-*`, `ui9-*`,
`diag-expand.png`. API probes are plain `curl` against `https://kotatsu.ppsk.localhost/api`.
