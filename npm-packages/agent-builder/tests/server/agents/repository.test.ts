import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { AgentDraft } from '@shared/agent';
import { AgentHasChatsError, AgentsRepository } from '../../../server/agents/repository';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

function setup(options: { now?: () => number } = {}) {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  return { database, repository: new AgentsRepository(database, options) };
}

function makeDraft(name: string): AgentDraft {
  return AgentDraft.parse({
    name,
    description: `${name} description`,
    config: {
      instructions: 'Be concise',
      provider: { id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' },
      metaHarness: { '01_intro': true },
      mcpServers: {
        docs: { type: 'http', url: 'https://example.com/mcp', headers: { Authorization: 'Bearer token' } },
        local: { type: 'stdio', command: 'node', args: ['server.js'], env: { TOKEN: 'secret' } },
      },
    },
  });
}

describe('AgentsRepository', () => {
  test('creates, reads, lists and deletes an agent', () => {
    const { repository } = setup();
    const draft = makeDraft('Reviewer');

    const created = repository.create(draft);
    expect(created.id).toMatch(/^[0-9a-f-]{36}$/);
    expect(created.createdAt).toBe(created.updatedAt);
    expect(created.config).toEqual(draft.config);

    expect(repository.get(created.id)).toEqual(created);
    expect(repository.list()).toEqual([
      {
        id: created.id,
        name: created.name,
        description: created.description,
        createdAt: created.createdAt,
        revision: 1,
        updatedAt: created.updatedAt,
      },
    ]);

    expect(repository.delete(created.id, created.revision)).toBe(true);
    expect(repository.get(created.id)).toBeNull();
    expect(repository.delete(created.id, created.revision)).toBe(false);
  });

  test('updates name, description and config while preserving createdAt', () => {
    let now = 1_000;
    const { repository } = setup({ now: () => now });
    const created = repository.create(makeDraft('Reviewer'));

    now = 2_000;
    const replacement = AgentDraft.parse({
      name: 'Reviewer v2',
      description: '',
      config: { instructions: 'New instructions' },
    });
    const updated = repository.update(created.id, replacement, created.revision);

    expect(updated).not.toBeNull();
    expect(updated?.name).toBe('Reviewer v2');
    expect(updated?.description).toBe('');
    expect(updated?.config.instructions).toBe('New instructions');
    expect(updated?.createdAt).toBe(1_000);
    expect(updated?.updatedAt).toBe(2_000);
    expect(repository.get(created.id)).toEqual(updated);
  });

  test('returns null or false for unknown agents', () => {
    const { repository } = setup();

    expect(repository.get('00000000-0000-4000-8000-000000000000')).toBeNull();
    expect(repository.update('00000000-0000-4000-8000-000000000000', makeDraft('Missing'))).toBeNull();
    expect(repository.delete('00000000-0000-4000-8000-000000000000')).toBe(false);
  });

  test('lists agents by updatedAt descending', () => {
    let now = 1_000;
    const { repository } = setup({ now: () => now });

    const first = repository.create(makeDraft('First'));
    now = 2_000;
    const second = repository.create(makeDraft('Second'));
    expect(repository.list().map((agent) => agent.id)).toEqual([second.id, first.id]);

    now = 3_000;
    repository.update(first.id, makeDraft('First updated'), first.revision);
    expect(repository.list().map((agent) => agent.id)).toEqual([first.id, second.id]);
  });

  test('round-trips nested MCP configuration', () => {
    const { repository } = setup();
    const draft = makeDraft('Round trip');

    const created = repository.create(draft);
    const loaded = repository.get(created.id);

    expect(loaded?.config.mcpServers).toEqual(draft.config.mcpServers);
    expect(loaded?.config.provider).toEqual(draft.config.provider);
    expect(loaded?.config.metaHarness).toEqual(draft.config.metaHarness);
  });

  test('throws for corrupt stored JSON without exposing its content', () => {
    const { database, repository } = setup();
    database
      .query(
        'INSERT INTO agents (id, name, description, config, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)',
      )
      .run('00000000-0000-4000-8000-000000000001', 'Broken', '', '{not-json', 1, 1);

    expect(() => repository.get('00000000-0000-4000-8000-000000000001')).toThrow(
      /Stored agent config is invalid/,
    );
  });

  test('throws for stored JSON that violates the contract', () => {
    const { database, repository } = setup();
    database
      .query(
        'INSERT INTO agents (id, name, description, config, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)',
      )
      .run('00000000-0000-4000-8000-000000000002', 'Broken', '', '{"instructions":42}', 1, 1);

    expect(() => repository.get('00000000-0000-4000-8000-000000000002')).toThrow(
      /Stored agent config is invalid/,
    );
  });

  test('refuses to delete an agent that still has chats', () => {
    const { database, repository } = setup();
    const created = repository.create(makeDraft('Has chats'));
    database
      .query('INSERT INTO chats (chat_id, agent_id, created_at, updated_at) VALUES (?, ?, ?, ?)')
      .run('chat-1', created.id, 1, 1);

    expect(() => repository.delete(created.id, created.revision)).toThrow(AgentHasChatsError);
    expect(repository.get(created.id)?.id).toBe(created.id);

    database.query('DELETE FROM chats WHERE chat_id = ?').run('chat-1');
    expect(repository.delete(created.id, created.revision)).toBe(true);
  });
});
