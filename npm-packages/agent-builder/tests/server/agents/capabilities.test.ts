import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { BareHarnessConfig } from '@peri-code/sdk';
import { AgentCapabilities } from '@shared/agent';
import { ProviderCreate } from '@shared/provider';
import type { Hono } from 'hono';
import { createApp } from '../../../server/app';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { ProvidersRepository } from '../../../server/providers/repository';
import { ProviderService } from '../../../server/providers/service';
import { testEnv } from '../helpers';

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

function setup(envOverrides: Parameters<typeof testEnv>[0] = {}): {
  app: Hono;
  env: ReturnType<typeof testEnv>;
  providers: ProviderService;
} {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  const env = testEnv(envOverrides);
  return {
    app: createApp({ env, db: database }),
    env,
    providers: new ProviderService(new ProvidersRepository(database)),
  };
}

describe('GET /api/capabilities', () => {
  test('reports metaHarness keys, send limit and provider availability', async () => {
    const { app, providers } = setup();
    providers.create(
      ProviderCreate.parse({
        name: 'A',
        type: 'anthropic',
        credential: { action: 'replace', value: crypto.randomUUID() },
      }),
    );
    const response = await app.request('/api/capabilities');

    expect(response.status).toBe(200);
    const body = AgentCapabilities.parse(await response.json());
    expect(body.metaHarnessKeys).toEqual(Object.keys(BareHarnessConfig));
    expect(body.metaHarnessKeys).toContain('01_intro');
    expect(body.sendTextMaxLength).toBe(32_768);
    expect(body.providers).toEqual([
      { type: 'anthropic', configured: true },
      { type: 'openai', configured: false },
    ]);
    expect(body.chatRuntime).toBe('not_configured');
    expect(body.chatRuntimeReasons).toContain('store_token_missing');
    expect(body.chatRuntimeReasons).toContain('rivet_storage_path_missing');
    expect(body.chatRuntimeReasons).not.toContain('provider_key_missing');
    expect(body.historyPersisted).toBe(false);
    expect(body.sandbox).toEqual({ mode: 'disabled', reasons: [] });
  });

  test('never serializes API keys or store tokens', async () => {
    const secret = crypto.randomUUID();
    const { app, providers } = setup({ storeToken: secret });
    for (const type of ['anthropic', 'openai']) {
      providers.create(
        ProviderCreate.parse({ name: type, type, credential: { action: 'replace', value: secret } }),
      );
    }
    const response = await app.request('/api/capabilities');
    const raw = await response.text();

    expect(response.status).toBe(200);
    expect(raw.includes(secret)).toBe(false);
    expect(raw).not.toContain('authToken');
    expect(raw).not.toContain('apiKey');
  });
});
