import type { Database } from 'bun:sqlite';
import type { Hono } from 'hono';
import { z } from 'zod';
import { createApp } from '../../../../server/app';
import { openDatabase } from '../../../../server/db/database';
import { migrate } from '../../../../server/db/migrate';
import { testEnv } from '../../helpers';

export const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;

export const VaultBody = z.strictObject({
  id: z.string().min(1),
  object: z.literal('vault'),
  name: z.string().nullable(),
  metadata: z.record(z.string(), z.string()),
  created_at: z.number().int(),
});

const CredentialNetworkingBody = z.union([
  z.strictObject({ type: z.literal('unrestricted') }),
  z.strictObject({ type: z.literal('limited'), allowed_hosts: z.array(z.string()) }),
]);

const CredentialAuthBody = z.union([
  z.strictObject({ type: z.literal('static_bearer'), mcp_server_url: z.string() }),
  z.strictObject({
    type: z.literal('environment_variable'),
    secret_name: z.string(),
    networking: CredentialNetworkingBody,
  }),
]);

export const CredentialBody = z.strictObject({
  id: z.string().min(1),
  object: z.literal('vault.credential'),
  vault_id: z.string().min(1),
  name: z.string(),
  auth: CredentialAuthBody,
  metadata: z.record(z.string(), z.string()),
  created_at: z.number().int(),
  updated_at: z.number().int(),
});

export function ListBody<T extends z.ZodType>(item: T) {
  return z.strictObject({
    object: z.literal('list'),
    data: z.array(item),
    first_id: z.string().nullable(),
    last_id: z.string().nullable(),
    has_more: z.boolean(),
  });
}

export const ErrorBody = z.strictObject({
  error: z.strictObject({
    code: z.string().min(1),
    message: z.string().min(1),
    param: z.string().nullable(),
    type: z.string().min(1),
  }),
});

export interface TestContext {
  readonly app: Hono;
  readonly db: Database;
}

export function createTestContext(): TestContext {
  const db = openDatabase(':memory:');
  migrate(db);
  return { app: createApp({ env: testEnv(), db }), db };
}

/** Calls the platform surface below `/v1` with the presence-only bearer token. */
export async function api(
  app: Hono,
  method: 'GET' | 'POST' | 'DELETE',
  path: string,
  body?: unknown,
): Promise<Response> {
  const init: RequestInit = { method, headers: { ...AUTHORIZATION } };
  if (body !== undefined) {
    init.headers = { ...AUTHORIZATION, 'content-type': 'application/json' };
    init.body = JSON.stringify(body);
  }
  return app.request(`/v1${path}`, init);
}

export async function readError(response: Response): Promise<z.infer<typeof ErrorBody>> {
  return ErrorBody.parse(await response.json());
}

export async function createVault(app: Hono, name = 'Test vault'): Promise<z.infer<typeof VaultBody>> {
  const response = await api(app, 'POST', '/vaults', { name });
  return VaultBody.parse(await response.json());
}
