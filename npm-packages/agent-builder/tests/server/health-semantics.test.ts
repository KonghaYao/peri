import type { Database } from 'bun:sqlite';
import { afterEach, expect, test } from 'bun:test';
import { HealthResponse } from '@shared/api';
import { createApp } from '../../server/app';
import type { ChatEngine } from '../../server/chats/engine';
import { openDatabase } from '../../server/db/database';
import { migrate } from '../../server/db/migrate';
import { AppError } from '../../server/errors';
import { testEnv } from './helpers';

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

function engineWithHealth(status: 'ready' | 'unavailable'): ChatEngine {
  return {
    status: { mode: 'wasm', reasons: [] },
    start: async () => {},
    health: () => ({ mode: 'embedded', status }),
    actor: () => {
      throw new AppError('engine_unavailable', 'unused');
    },
    sandbox: () => null,
    endpoint: null,
    shutdown: async () => {},
  };
}

test('ok reports process liveness while engine.status owns chat readiness', async () => {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  const app = createApp({
    env: testEnv({ storeToken: 'store-token', rivetStoragePath: '/tmp/rivet' }),
    db: database,
    engine: engineWithHealth('unavailable'),
  });

  const response = await app.request('/api/health');
  expect(response.status).toBe(200);
  const body = HealthResponse.parse(await response.json());

  // The process answered the HTTP request; that is all `ok` promises.
  expect(body.ok).toBe(true);
  expect(body.engine).toEqual({ mode: 'embedded', status: 'unavailable' });
  expect(body.runtime.name).toBe('bun');
});
