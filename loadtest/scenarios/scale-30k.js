// scale-30k.js — Read performance at large subject scale (WS-K2, August 2026 QA plan).
//
// Seeds up to SCALE_TARGET subjects (default 30 000, deterministic, 60/25/15
// Avro/JSON/Proto mix — same shape as helpers.js) then measures the read path at
// that scale. Seeding is IDEMPOTENT: it only registers what's missing, so re-runs
// are fast and the DB volume can be snapshotted between runs.
//
// All data is synthetic (`scale-*-{i}`) — no client data.
//
//   KORA_URL         Kora base URL (direct or via traefik with creds in the URL)
//   SCALE_TARGET     subjects to reach (default 30000)
//   SCALE_VUS        concurrent readers (default 50)
//   SCALE_DURATION   measurement duration (default 3m)

import { check } from 'k6';
import {
  registerSchema, getByVersion, listVersions, listSubjects, checkSchema, getById,
} from '../helpers.js';

const TARGET = Number(__ENV.SCALE_TARGET || 30000);
const SEED_LOG_EVERY = 1000;

// Deterministic subject generator — mirrors the 60/25/15 corpus mix in helpers.js.
export function makeSubject(i) {
  const mod = i % 20;
  if (mod < 12) {
    // 60% Avro
    return {
      subject: `scale-avro-${i}`,
      schemaType: 'AVRO',
      schema: JSON.stringify({
        type: 'record', name: `ScaleAvro${i}`, namespace: 'kora.scale',
        fields: [{ name: 'id', type: 'long' }, { name: `f_${i}`, type: 'string' }],
      }),
    };
  }
  if (mod < 17) {
    // 25% JSON Schema
    return {
      subject: `scale-json-${i}`,
      schemaType: 'JSON',
      schema: JSON.stringify({
        type: 'object',
        properties: { id: { type: 'integer' }, [`f_${i}`]: { type: 'string' } },
        required: ['id'],
      }),
    };
  }
  // 15% Protobuf
  return {
    subject: `scale-proto-${i}`,
    schemaType: 'PROTOBUF',
    schema: `syntax = "proto3";\nmessage ScaleProto${i} {\n  int64 id = 1;\n  string f_${i} = 2;\n}`,
  };
}

export const options = {
  setupTimeout: __ENV.SCALE_SETUP_TIMEOUT || '60m', // 30k sequential registrations is slow — seed once, snapshot the DB.
  scenarios: {
    reads_at_scale: {
      executor: 'constant-vus',
      vus: Number(__ENV.SCALE_VUS || 50),
      duration: __ENV.SCALE_DURATION || '3m',
    },
  },
  // Provisional p99 gates — TIGHTEN from the WS-K1 baseline before treating as SLA.
  thresholds: {
    http_req_failed: ['rate==0'],
    'http_req_duration{op:get_by_version}': ['p(99)<150'],
    'http_req_duration{op:list_versions}': ['p(99)<150'],
    'http_req_duration{op:check_schema}': ['p(99)<200'],
    'http_req_duration{op:list_subjects}': ['p(99)<500'], // the heavy scan at scale
    'http_req_duration{op:get_by_id}': ['p(99)<150'],
  },
};

export function setup() {
  // Idempotent seed: register only the missing subjects so re-runs are cheap.
  let have = 0;
  const res = listSubjects();
  try { have = res.json().length; } catch (_) { have = 0; }

  if (have >= TARGET) {
    console.log(`already at ${have} subjects (>= ${TARGET}) — skipping seed`);
    return { target: TARGET, seeded: false, sampleIds: [] };
  }

  console.log(`seeding subjects ${have} -> ${TARGET}`);
  const sampleIds = []; // keep every 100th id to exercise GET /schemas/ids/{id}
  for (let i = have; i < TARGET; i++) {
    const s = makeSubject(i);
    const r = registerSchema(s.subject, s.schema, s.schemaType);
    if (r.status === 200 && i % 100 === 0) {
      try { sampleIds.push(r.json().id); } catch (_) { /* ignore */ }
    }
    if (i % SEED_LOG_EVERY === 0) console.log(`  seeded ${i}/${TARGET}`);
  }
  return { target: TARGET, seeded: true, sampleIds };
}

export default function (data) {
  const i = Math.floor(Math.random() * data.target);
  const s = makeSubject(i);

  check(getByVersion(s.subject), { 'get_by_version: 200': (r) => r.status === 200 });
  check(listVersions(s.subject), { 'list_versions: 200': (r) => r.status === 200 });
  check(checkSchema(s.subject, s.schema, s.schemaType), { 'check_schema: 200': (r) => r.status === 200 });

  // The list scan at scale — prefix-scoped to stay realistic.
  check(listSubjects('scale-avro-'), { 'list_subjects: 200': (r) => r.status === 200 });

  if (data.sampleIds && data.sampleIds.length) {
    const id = data.sampleIds[Math.floor(Math.random() * data.sampleIds.length)];
    check(getById(id), { 'get_by_id: 200': (r) => r.status === 200 });
  }
}
