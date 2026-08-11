// stress-hard.js — Push past stress.js (300 VUs) to actually find the knee.
// Ramps aggressively to 1000 VUs. Same read/write/compat mix as stress.js.
// Expect degradation and possibly errors near the top — that's the point.

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
        { duration: '30s', target: 100 },
        { duration: '1m', target: 300 },
        { duration: '1m', target: 500 },
        { duration: '1m', target: 800 },
        { duration: '1m', target: 1000 },  // top
        { duration: '1m', target: 1000 },  // sustained peak
        { duration: '30s', target: 0 },    // cool-down
      ],
    },
  },

  thresholds: {
    // Very relaxed — we are deliberately looking for the breaking point.
    http_req_failed: ['rate<0.10'],
    'http_req_duration{op:get_by_id}': ['p(95)<2000'],
    'http_req_duration{op:register}': ['p(95)<5000'],
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
    check(getById(entry.id), {
      'stress: get_by_id ok': (r) => r.status === 200,
    });
    check(getByVersion(entry.subject), {
      'stress: get_by_version ok': (r) => r.status === 200,
    });
  } else if (roll < 0.85) {
    // 25% writes
    const idx = Math.floor(Math.random() * SCHEMAS.length);
    const base = SCHEMAS[idx];
    const uniqueSubject = `stresshard-${base.subject}-${__VU}-${__ITER}`;
    check(registerSchema(uniqueSubject, base.schema, base.schemaType), {
      'stress: register ok': (r) => r.status === 200,
    });
  } else {
    // 15% compat checks
    const entry = data.seeded[Math.floor(Math.random() * data.seeded.length)];
    if (entry.schemaType === 'AVRO') {
      const evolved = evolveAvro(
        entry.subject.replace('avro-perf-', 'AvroRec'),
        `stresshard_${__VU}_${__ITER}`,
      );
      check(testCompatibility(entry.subject, 'latest', evolved), {
        'stress: compat ok': (r) => r.status === 200,
      });
    }
  }

  sleep(0.1);
}
