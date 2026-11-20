import type { Database } from 'bun:sqlite';
import { ProviderType } from '@shared/provider';
import type { Hono } from 'hono';
import { z } from 'zod';
import { createApp } from '../../../../server/app';
import { openDatabase } from '../../../../server/db/database';
import { migrate } from '../../../../server/db/migrate';
import { ProvidersRepository } from '../../../../server/providers/repository';
import { testEnv } from '../../helpers';

export const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;

export interface AgentTestContext {
  readonly app: Hono;
  readonly db: Database;
}

export function setupAgentApp(): AgentTestContext {
  const db = openDatabase(':memory:');
  migrate(db);
  return { app: createApp({ env: testEnv(), db }), db };
}

/** Any valid provider protocol works; agent model resolution keys off the provider name. */
const PROVIDER_TYPE = ProviderType.options[0];

export function seedProvider(ctx: AgentTestContext, name: string): string {
  if (PROVIDER_TYPE === undefined) throw new Error('ProviderType has no values');
  const id = crypto.randomUUID();
  new ProvidersRepository(ctx.db).create({
    id,
    name,
    type: PROVIDER_TYPE,
    baseUrl: null,
    credentialSource: 'env',
    apiKey: null,
    revision: 1,
  });
  return id;
}

export function postJson(body: unknown): RequestInit {
  return {
    method: 'POST',
    headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
    body: JSON.stringify(body),
  };
}

export function deleteRequest(): RequestInit {
  return { method: 'DELETE', headers: AUTHORIZATION };
}

export const JsonObject = z.record(z.string(), z.unknown());

/** Local mirror of the documented wire shape, including strictness about extra fields. */
export const AgentResource = z.strictObject({
  id: z.string().min(1),
  object: z.literal('agent'),
  name: z.string().nullable(),
  instructions: z.string().nullable(),
  model: z.string(),
  metadata: z.record(z.string(), z.string()),
  reasoning: z.strictObject({
    effort: z.string().nullable(),
    summary: z.string().nullable(),
  }),
  text: z.strictObject({
    format: z.union([
      z.strictObject({ type: z.literal('text') }),
      z.strictObject({ type: z.literal('json_schema'), schema: JsonObject }),
    ]),
    verbosity: z.enum(['low', 'medium', 'high']),
  }),
  tools: z.array(JsonObject),
  multi_agent: z.strictObject({
    enabled: z.boolean(),
    max_concurrent_subagents: z.number().int().nullable(),
  }),
  compaction: z.strictObject({ async: z.boolean() }),
  experimental: z.strictObject({ time_awareness: z.boolean() }),
  service_tier: z.string(),
  created_at: z.number().int(),
  updated_at: z.number().int(),
});

export const AgentListResource = z.strictObject({
  object: z.literal('list'),
  data: z.array(AgentResource),
  first_id: z.string().nullable(),
  last_id: z.string().nullable(),
  has_more: z.boolean(),
});

export const PlatformErrorBody = z.strictObject({
  error: z.strictObject({
    type: z.string(),
    code: z.string(),
    message: z.string(),
    param: z.string().nullable(),
  }),
});
