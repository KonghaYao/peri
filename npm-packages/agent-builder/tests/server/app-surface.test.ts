import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import type { Hono } from 'hono';
import { createApp } from '../../server/app';
import { openDatabase } from '../../server/db/database';
import { migrate } from '../../server/db/migrate';
import { testEnv } from './helpers';

const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;

interface PlatformErrorBody {
  readonly error: {
    readonly type: string;
    readonly code: string;
    readonly message: string;
    readonly param: string | null;
  };
}

interface ApiErrorBody {
  readonly error: {
    readonly code: string;
    readonly message: string;
    readonly retryable: boolean;
  };
}

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

function setup(): Hono {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  return createApp({ env: testEnv(), db: database });
}

async function platformBody(response: Response): Promise<PlatformErrorBody> {
  return (await response.json()) as PlatformErrorBody;
}

describe('application surface', () => {
  test('does not serve the removed /api and browser surfaces', async () => {
    const app = setup();

    for (const [method, path] of [
      ['GET', '/api/health'],
      ['GET', '/api/chats'],
      ['POST', '/api/chats'],
      ['GET', '/'],
      ['GET', '/index.html'],
    ] as const) {
      const response = await app.request(path, { method });
      expect({ path, status: response.status }).toEqual({ path, status: 404 });
      const body = (await response.json()) as ApiErrorBody;
      expect(body.error.code).toBe('not_found');
      expect(body.error.retryable).toBe(false);
    }
  });

  test('never advertises degradation through a response header', async () => {
    const app = setup();
    const responses = [
      await app.request('/v1/agents', { headers: AUTHORIZATION }),
      await app.request('/v1/agents'),
      await app.request('/v1/agents/sessions', { headers: AUTHORIZATION }),
      await app.request('/api/health'),
      await app.request('/'),
      await app.request('/v1/unknown', { headers: AUTHORIZATION }),
      await app.request('/v1/agents', {
        method: 'POST',
        headers: {
          ...AUTHORIZATION,
          'content-type': 'application/json',
          origin: 'https://evil.example',
        },
        body: '{}',
      }),
    ];

    expect(responses.map((response) => response.status)).toEqual([200, 401, 501, 404, 404, 404, 403]);
    for (const response of responses) {
      expect(response.headers.has('x-platform-degraded')).toBe(false);
    }
  });

  test('maps capabilities this deployment cannot execute to the canonical 501', async () => {
    const app = setup();

    for (const [method, path] of [
      ['GET', '/v1/agents/sessions'],
      ['POST', '/v1/agents/sessions'],
      ['GET', '/v1/agents/sessions/session_sample/events'],
      ['GET', '/v1/agents/sessions/session_sample/turns'],
    ] as const) {
      const response = await app.request(path, {
        method,
        headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
        ...(method === 'POST' ? { body: '{}' } : {}),
      });
      expect({ path, status: response.status }).toEqual({ path, status: 501 });
      const body = await platformBody(response);
      expect(body.error).toMatchObject({
        type: 'not_implemented_error',
        code: 'not_implemented',
        param: null,
      });
    }

    const hosted = await app.request('/v1/agents/environments', {
      method: 'POST',
      headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
      body: JSON.stringify({ environment: { type: 'openai_hosted' } }),
    });
    expect(hosted.status).toBe(501);
    const hostedBody = await platformBody(hosted);
    expect(hostedBody.error).toMatchObject({
      type: 'not_implemented_error',
      code: 'not_implemented',
      param: 'type',
    });
  });
});
