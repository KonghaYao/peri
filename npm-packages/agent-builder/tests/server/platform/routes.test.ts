import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import type { Hono } from 'hono';
import { z } from 'zod';
import { createApp } from '../../../server/app';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { testEnv } from '../helpers';

const PlatformErrorBody = z.strictObject({
  error: z.strictObject({
    code: z.string().min(1),
    message: z.string().min(1),
    param: z.string().nullable(),
    type: z.string().min(1),
  }),
});

interface Operation {
  readonly method: 'GET' | 'POST' | 'DELETE';
  /** Path below `/v1`; braces mark path parameters. */
  readonly path: string;
}

/** Mirrors the documented operation inventory: 57 operations over 34 paths. */
const OPERATIONS: readonly Operation[] = [
  { method: 'POST', path: '/agents/automations' },
  { method: 'GET', path: '/agents/automations' },
  { method: 'GET', path: '/agents/automations/{automation_id}' },
  { method: 'POST', path: '/agents/automations/{automation_id}' },
  { method: 'DELETE', path: '/agents/automations/{automation_id}' },
  { method: 'POST', path: '/agents/automations/{automation_id}/pause' },
  { method: 'POST', path: '/agents/automations/{automation_id}/resume' },
  { method: 'POST', path: '/agents/automations/{automation_id}/invocations' },
  { method: 'GET', path: '/agents/automations/{automation_id}/invocations' },
  {
    method: 'GET',
    path: '/agents/automations/{automation_id}/invocations/{invocation_id}',
  },
  { method: 'GET', path: '/agents/environments' },
  { method: 'POST', path: '/agents/environments' },
  { method: 'GET', path: '/agents/environments/{environment_id}' },
  { method: 'GET', path: '/agents/environments/{environment_id}/files' },
  { method: 'POST', path: '/agents/environments/{environment_id}/files' },
  { method: 'GET', path: '/agents/sessions/{session_id}/subagents' },
  { method: 'GET', path: '/agents/sessions/{session_id}/subagents/{subagent_id}' },
  { method: 'GET', path: '/agents/sessions/{session_id}/subagents/{subagent_id}/items' },
  { method: 'GET', path: '/agents/sessions/{session_id}/subagents/{subagent_id}/turns' },
  {
    method: 'GET',
    path: '/agents/sessions/{session_id}/subagents/{subagent_id}/turns/{turn_id}',
  },
  {
    method: 'GET',
    path: '/agents/sessions/{session_id}/subagents/{subagent_id}/turns/{turn_id}/items',
  },
  { method: 'GET', path: '/agents' },
  { method: 'POST', path: '/agents' },
  { method: 'GET', path: '/agents/{agent_id}' },
  { method: 'POST', path: '/agents/{agent_id}' },
  { method: 'DELETE', path: '/agents/{agent_id}' },
  { method: 'GET', path: '/agents/environments/templates' },
  { method: 'POST', path: '/agents/environments/templates' },
  { method: 'GET', path: '/agents/environments/templates/{environment_template_id}' },
  { method: 'POST', path: '/agents/environments/templates/{environment_template_id}' },
  { method: 'DELETE', path: '/agents/environments/templates/{environment_template_id}' },
  { method: 'GET', path: '/agents/sessions' },
  { method: 'POST', path: '/agents/sessions' },
  { method: 'GET', path: '/agents/sessions/{session_id}' },
  { method: 'POST', path: '/agents/sessions/{session_id}' },
  { method: 'DELETE', path: '/agents/sessions/{session_id}' },
  { method: 'GET', path: '/agents/sessions/{session_id}/artifacts' },
  { method: 'GET', path: '/agents/sessions/{session_id}/artifacts/{artifact_id}' },
  { method: 'DELETE', path: '/agents/sessions/{session_id}/artifacts/{artifact_id}' },
  { method: 'GET', path: '/agents/sessions/{session_id}/artifacts/{artifact_id}/content' },
  { method: 'GET', path: '/agents/sessions/{session_id}/events' },
  { method: 'POST', path: '/agents/sessions/{session_id}/events' },
  { method: 'GET', path: '/agents/sessions/{session_id}/items' },
  { method: 'GET', path: '/agents/sessions/{session_id}/traces' },
  { method: 'GET', path: '/agents/sessions/{session_id}/turns' },
  { method: 'GET', path: '/agents/sessions/{session_id}/turns/{turn_id}' },
  { method: 'GET', path: '/agents/sessions/{session_id}/turns/{turn_id}/items' },
  { method: 'GET', path: '/vaults' },
  { method: 'POST', path: '/vaults' },
  { method: 'GET', path: '/vaults/{vault_id}' },
  { method: 'POST', path: '/vaults/{vault_id}' },
  { method: 'DELETE', path: '/vaults/{vault_id}' },
  { method: 'GET', path: '/vaults/{vault_id}/credentials' },
  { method: 'POST', path: '/vaults/{vault_id}/credentials' },
  { method: 'GET', path: '/vaults/{vault_id}/credentials/{credential_id}' },
  { method: 'POST', path: '/vaults/{vault_id}/credentials/{credential_id}' },
  { method: 'DELETE', path: '/vaults/{vault_id}/credentials/{credential_id}' },
];

/**
 * Formerly scaffolded operations now have real handlers. They are asserted below through a
 * missing resource so the test proves the handler is wired instead of encoding progress.
 */
const FORMER_SCAFFOLD_OPERATIONS: ReadonlySet<string> = new Set([
  'POST /vaults/{vault_id}',
  'POST /vaults/{vault_id}/credentials/{credential_id}',
]);

/** Every documented operation has a handler in this deployment. */
const IMPLEMENTED_OPERATIONS: ReadonlySet<string> = new Set(
  OPERATIONS.map((operation) => `${operation.method} ${operation.path}`),
);

const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;

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

function requestPath(path: string): string {
  return `/v1${path.replace(/\{([a-z_]+)\}/g, (_match, name: string) => `${name}_sample`)}`;
}

function routePattern(path: string): string {
  return `/v1${path.replace(/\{([a-z_]+)\}/g, ':$1')}`;
}

describe('platform route registration', () => {
  test('registers all documented methods and paths', () => {
    const app = setup();

    expect(OPERATIONS).toHaveLength(57);
    expect(new Set(OPERATIONS.map((operation) => operation.path)).size).toBe(34);

    const registered = new Set(
      app.routes
        .filter((route) => route.method !== 'ALL' && route.path.startsWith('/v1/'))
        .map((route) => `${route.method} ${route.path}`),
    );

    const missing = OPERATIONS.filter(
      (operation) => !registered.has(`${operation.method} ${routePattern(operation.path)}`),
    );
    expect(missing).toEqual([]);
  });

  test('answers former scaffold operations through their real handlers', async () => {
    const app = setup();

    for (const key of FORMER_SCAFFOLD_OPERATIONS) {
      const path = key.split(' ')[1] ?? '';
      const response = await app.request(requestPath(path), {
        method: 'POST',
        headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
        body: '{}',
      });
      // A real handler reports the missing vault; the shared scaffold would answer 501 instead.
      expect(response.status).toBe(404);
      const body = PlatformErrorBody.parse(await response.json());
      expect(body.error.code).toBe('not_found');
      expect(body.error.type).toBe('not_found_error');
    }
  });

  test('rejects a wrong method with a canonical 404 instead of not_implemented', async () => {
    const app = setup();
    const response = await app.request('/v1/agents/agent_id_sample', {
      method: 'PUT',
      headers: AUTHORIZATION,
    });

    expect(response.status).toBe(404);
    expect(PlatformErrorBody.parse(await response.json()).error.code).toBe('not_found');
  });

  test('returns a canonical 404 for unknown platform paths', async () => {
    const app = setup();
    const response = await app.request('/v1/unknown', { headers: AUTHORIZATION });

    expect(response.status).toBe(404);
    const body = PlatformErrorBody.parse(await response.json());
    expect(body.error.type).toBe('not_found_error');
    expect(body.error.param).toBeNull();
  });
});

describe('platform authorization', () => {
  test('returns a canonical 401 when Authorization is missing', async () => {
    const app = setup();
    const response = await app.request('/v1/agents');

    expect(response.status).toBe(401);
    const body = PlatformErrorBody.parse(await response.json());
    expect(body.error).toEqual({
      type: 'authentication_error',
      code: 'authentication_required',
      message: 'An Authorization header with a bearer token is required',
      param: null,
    });
  });

  test('rejects a non-bearer scheme', async () => {
    const app = setup();
    const response = await app.request('/v1/vaults', {
      headers: { authorization: 'Basic dXNlcjpwYXNz' },
    });

    expect(response.status).toBe(401);
    expect(PlatformErrorBody.parse(await response.json()).error.type).toBe('authentication_error');
  });

  test('accepts any bearer token without recording it', async () => {
    const app = setup();
    const response = await app.request('/v1/agents', {
      headers: { authorization: 'Bearer arbitrary-value' },
    });

    expect(response.status).toBe(200);
    expect(await response.text()).not.toContain('arbitrary-value');
  });

  test('never answers 401 on an implemented operation for any bearer token', async () => {
    const app = setup();

    for (const operation of OPERATIONS) {
      if (!IMPLEMENTED_OPERATIONS.has(`${operation.method} ${operation.path}`)) continue;
      const response = await app.request(requestPath(operation.path), {
        method: operation.method,
        headers: { authorization: `Bearer ${operation.method}-token` },
      });
      expect({ operation, status: response.status }).not.toEqual({ operation, status: 401 });
    }
  });
});
