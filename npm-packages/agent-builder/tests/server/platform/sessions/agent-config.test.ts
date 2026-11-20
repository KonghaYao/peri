import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { ProviderCreate } from '@shared/provider';
import { openDatabase } from '../../../../server/db/database';
import { migrate } from '../../../../server/db/migrate';
import { AppError } from '../../../../server/errors';
import { PlatformAgentsRepository } from '../../../../server/platform/repositories/agents';
import { resolveSessionAgent } from '../../../../server/platform/sessions/agent-config';
import { ProvidersRepository } from '../../../../server/providers/repository';
import { ProviderService } from '../../../../server/providers/service';

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

function setup(): { db: Database; agents: PlatformAgentsRepository; providers: ProviderService } {
  db = openDatabase(':memory:');
  migrate(db);
  return {
    db,
    agents: new PlatformAgentsRepository(db),
    providers: new ProviderService(new ProvidersRepository(db)),
  };
}

function addProvider(providers: ProviderService, name: string): string {
  return providers.create(
    ProviderCreate.parse({
      name,
      type: 'anthropic',
      baseUrl: 'http://127.0.0.1:9/v1',
      credential: { action: 'replace', value: 'test-key' },
    }),
  ).id;
}

function codeOf(run: () => unknown): string {
  try {
    run();
  } catch (error) {
    if (error instanceof AppError) return error.code;
    throw error;
  }
  throw new Error('expected the call to fail');
}

describe('session agent provider resolution', () => {
  test('a saved agent without a provider resolves through the only configured Provider', () => {
    const { agents, providers } = setup();
    const agent = agents.create({ name: 'Saved', model: 'test-model' });
    const providerId = addProvider(providers, 'only');

    const resolved = resolveSessionAgent({ agents, providers, agentId: agent.id });
    expect(resolved.snapshot.draft.config.provider).toEqual({
      kind: 'reference',
      providerId,
      model: 'test-model',
    });
    expect(resolved.snapshot.agent.model).toBe('test-model');
  });

  test('an inline agent uses the same unambiguous-deployment rule', () => {
    const { agents, providers } = setup();
    const providerId = addProvider(providers, 'only');

    const resolved = resolveSessionAgent({ agents, providers, agent: { model: 'test-model' } });
    expect(resolved.snapshot.draft.config.provider).toEqual({
      kind: 'reference',
      providerId,
      model: 'test-model',
    });
  });

  test('no configured Provider fails instead of guessing', () => {
    const { agents, providers } = setup();
    const agent = agents.create({ name: 'Saved', model: 'test-model' });

    expect(codeOf(() => resolveSessionAgent({ agents, providers, agentId: agent.id }))).toBe('invalid_input');
    expect(codeOf(() => resolveSessionAgent({ agents, providers, agent: { model: 'test-model' } }))).toBe(
      'invalid_input',
    );
  });

  test('several configured Providers fail instead of picking one', () => {
    const { agents, providers } = setup();
    const agent = agents.create({ name: 'Saved', model: 'test-model' });
    addProvider(providers, 'first');
    addProvider(providers, 'second');

    expect(codeOf(() => resolveSessionAgent({ agents, providers, agentId: agent.id }))).toBe('invalid_input');
  });
});
