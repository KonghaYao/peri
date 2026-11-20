import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { ConfigError, configFailure } from '../../../server/config-error';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { loadEnv, type ServerEnv } from '../../../server/env';
import { seedProviderFromEnv } from '../../../server/providers/env-provider';
import { type ProviderRecord, ProvidersRepository } from '../../../server/providers/repository';
import { testEnv } from '../helpers';

const roots: string[] = [];
const databases: Database[] = [];

afterEach(() => {
  for (const db of databases.splice(0)) db.close();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function database(): Database {
  const db = openDatabase(':memory:');
  migrate(db);
  databases.push(db);
  return db;
}

function env(provider: Partial<ServerEnv['provider']> = {}): ServerEnv {
  return testEnv({
    provider: {
      type: undefined,
      baseUrl: undefined,
      apiKey: undefined,
      name: 'env',
      ...provider,
    },
  });
}

function row(record: Omit<ProviderRecord, 'revision'>): ProviderRecord {
  return { ...record, revision: 1 };
}

describe('seedProviderFromEnv', () => {
  test('does nothing when PERI_PROVIDER_TYPE is absent, even with other provider values set', () => {
    const db = database();
    const result = seedProviderFromEnv(
      db,
      env({ baseUrl: 'https://api.example.com/v1', apiKey: crypto.randomUUID() }),
    );

    expect(result).toBeNull();
    expect(new ProvidersRepository(db).list()).toEqual([]);
  });

  test('inserts exactly one local provider and is idempotent across restarts', () => {
    const db = database();
    const providerEnv = env({
      type: 'openai',
      baseUrl: 'https://api.example.com/v1',
      apiKey: crypto.randomUUID(),
      name: 'deployment',
    });

    expect(seedProviderFromEnv(db, providerEnv)).toEqual({ type: 'openai', action: 'inserted' });
    const [created] = new ProvidersRepository(db).list();
    if (created === undefined) throw new Error('expected one seeded provider');
    expect(created).toMatchObject({
      name: 'deployment',
      type: 'openai',
      baseUrl: 'https://api.example.com/v1',
      credentialSource: 'local',
      apiKey: providerEnv.provider.apiKey,
      revision: 1,
    });

    expect(seedProviderFromEnv(db, providerEnv)).toEqual({ type: 'openai', action: 'unchanged' });
    expect(new ProvidersRepository(db).list()).toEqual([created]);
  });

  test('updates one existing row in place, including its type, without changing its id', () => {
    const db = database();
    const repository = new ProvidersRepository(db);
    const existing = row({
      id: crypto.randomUUID(),
      name: 'old',
      type: 'anthropic',
      baseUrl: null,
      credentialSource: 'local',
      apiKey: null,
    });
    repository.create(existing);

    const result = seedProviderFromEnv(
      db,
      env({ type: 'openai', baseUrl: 'https://api.example.com/v2', apiKey: 'replacement', name: 'new' }),
    );

    expect(result).toEqual({ type: 'openai', action: 'updated' });
    expect(repository.get(existing.id)).toEqual({
      id: existing.id,
      name: 'new',
      type: 'openai',
      baseUrl: 'https://api.example.com/v2',
      credentialSource: 'local',
      apiKey: 'replacement',
      revision: 2,
    });
  });

  test('a second run with changed env values updates the seeded row instead of adding another', () => {
    const db = database();
    const repository = new ProvidersRepository(db);
    expect(
      seedProviderFromEnv(
        db,
        env({
          type: 'anthropic',
          baseUrl: 'https://api.example.com/v1',
          apiKey: 'first-key',
          name: 'deployment',
        }),
      ),
    ).toEqual({ type: 'anthropic', action: 'inserted' });
    const [seeded] = repository.list();
    if (seeded === undefined) throw new Error('expected one seeded provider');

    expect(
      seedProviderFromEnv(
        db,
        env({
          type: 'openai',
          baseUrl: 'https://api.example.com/v2',
          apiKey: 'second-key',
          name: 'deployment-2',
        }),
      ),
    ).toEqual({ type: 'openai', action: 'updated' });
    const rows = repository.list();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toEqual({
      ...seeded,
      name: 'deployment-2',
      type: 'openai',
      baseUrl: 'https://api.example.com/v2',
      apiKey: 'second-key',
      revision: 2,
    });
  });

  test('updates a legacy env row to local when the environment supplies a key', () => {
    const db = database();
    const repository = new ProvidersRepository(db);
    const existing = row({
      id: crypto.randomUUID(),
      name: 'legacy',
      type: 'anthropic',
      baseUrl: null,
      credentialSource: 'env',
      apiKey: null,
    });
    repository.create(existing);

    expect(seedProviderFromEnv(db, env({ type: 'anthropic', apiKey: 'env-key' }))).toEqual({
      type: 'anthropic',
      action: 'updated',
    });
    expect(repository.get(existing.id)).toMatchObject({
      id: existing.id,
      credentialSource: 'local',
      apiKey: 'env-key',
    });
  });

  test('rejects multiple rows with a stable config code and never deletes them', () => {
    const db = database();
    const repository = new ProvidersRepository(db);
    for (const type of ['anthropic', 'openai'] as const) {
      repository.create(
        row({
          id: crypto.randomUUID(),
          name: type,
          type,
          baseUrl: null,
          credentialSource: 'local',
          apiKey: null,
        }),
      );
    }

    let thrown: unknown;
    try {
      seedProviderFromEnv(db, env({ type: 'openai' }));
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(ConfigError);
    expect((thrown as ConfigError).code).toBe('provider_env_conflict');
    expect(configFailure(thrown)?.hint).toContain('PERI_PROVIDER_TYPE');
    expect(configFailure(thrown)?.hint).toContain('PERI_PROVIDER_API_KEY');
    expect(repository.list()).toHaveLength(2);
  });
});

describe('provider environment', () => {
  test('exposes a frozen provider section with the env name default', () => {
    const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-provider-env-')));
    roots.push(root);
    const loaded = loadEnv(
      {
        DATABASE_PATH: path.join(root, 'agent-builder.db'),
        PERI_WORKSPACE_PATH: process.cwd(),
        PERI_SANDBOX_PERI_PATH: path.join(root, 'no-peri'),
        PERI_PROVIDER_TYPE: 'openai',
      },
      { identityDir: root },
    );

    expect(loaded.provider).toEqual({
      type: 'openai',
      baseUrl: undefined,
      apiKey: undefined,
      name: 'env',
    });
    expect(Object.isFrozen(loaded.provider)).toBe(true);
  });

  test('names an invalid provider type without echoing the value', () => {
    const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-provider-env-')));
    roots.push(root);

    let thrown: unknown;
    try {
      loadEnv(
        {
          DATABASE_PATH: path.join(root, 'agent-builder.db'),
          PERI_WORKSPACE_PATH: process.cwd(),
          PERI_SANDBOX_PERI_PATH: path.join(root, 'no-peri'),
          PERI_PROVIDER_TYPE: 'gemini',
        },
        { identityDir: root },
      );
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(ConfigError);
    expect((thrown as ConfigError).code).toBe('config_invalid');
    expect((thrown as ConfigError).fields).toEqual(['PERI_PROVIDER_TYPE']);
    expect((thrown as Error).message).toBe('config_invalid');
  });

  test('rejects an invalid provider base URL as invalid configuration', () => {
    const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-provider-env-')));
    roots.push(root);

    let thrown: unknown;
    try {
      loadEnv(
        {
          DATABASE_PATH: path.join(root, 'agent-builder.db'),
          PERI_WORKSPACE_PATH: process.cwd(),
          PERI_SANDBOX_PERI_PATH: path.join(root, 'no-peri'),
          PERI_PROVIDER_TYPE: 'openai',
          PERI_PROVIDER_BASE_URL: 'http://api.example.com/v1',
        },
        { identityDir: root },
      );
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(ConfigError);
    expect((thrown as ConfigError).code).toBe('config_invalid');
    expect((thrown as ConfigError).fields).toEqual(['PERI_PROVIDER_BASE_URL']);
  });
});
