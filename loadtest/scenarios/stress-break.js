// stress-break.js — Go find the actual breaking point.
// Ramps in wide stages up to 5000 VUs so we can see AT WHICH LEVEL errors/
// collapse start. No abortOnFail — we want the run to complete through the break.

import { check, sleep } from 'k6';
import {
  seedSchemas, getById, getByVersion, registerSchema, testCompatibility,
  evolveAvro, SCHEMAS,
} from '../helpers.js';

export const options = {
  setupTimeout: '120s',
  scenarios: {
    stress_mix: {
      executor: 'ramping-vus',
      startVUs: 0,
      stages: [
        { duration: '30s', target: 500 },
        { duration: '1m', target: 1000 },
        { duration: '1m', target: 1500 },
        { duration: '1m', target: 2000 },
        { duration: '1m', target: 2500 },  // top (host-RAM bounded on this laptop)
        { duration: '1m', target: 2500 },  // sustained
        { duration: '30s', target: 0 },
      ],
    },
  },
  // Report-only thresholds (won't abort the run): we're hunting the break.
  thresholds: {
    http_req_failed: ['rate<0.99'],
  },
};

export function setup() {
  const seeded = seedSchemas(300);
  return { seeded };
}

export default function (data) {
  const roll = Math.random();

  if (roll < 0.60) {
    // 60% reads
    const entry = data.seeded[Math.floor(Math.random() * data.seeded.length)];
    check(getById(entry.id), { 'stress: get_by_id ok': (r) => r.status === 200 });
    check(getByVersion(entry.subject), { 'stress: get_by_version ok': (r) => r.status === 200 });
  } else if (roll < 0.85) {
    // 25% writes
    const idx = Math.floor(Math.random() * SCHEMAS.length);
    const base = SCHEMAS[idx];
    const uniqueSubject = `stressbreak-${base.subject}-${__VU}-${__ITER}`;
    check(registerSchema(uniqueSubject, base.schema, base.schemaType), {
      'stress: register ok': (r) => r.status === 200,
    });
  } else {
    // 15% compat checks
    const entry = data.seeded[Math.floor(Math.random() * data.seeded.length)];
    if (entry.schemaType === 'AVRO') {
      const evolved = evolveAvro(
        entry.subject.replace('avro-perf-', 'AvroRec'),
        `stressbreak_${__VU}_${__ITER}`,
      );
      check(testCompatibility(entry.subject, 'latest', evolved), {
        'stress: compat ok': (r) => r.status === 200,
      });
    }
  }

  sleep(0.1);
}
