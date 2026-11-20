import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { z } from 'zod';
import { openDatabase } from '../../../server/db/database';
import {
  getSchemaVersion,
  LATEST_SCHEMA_VERSION,
  migrate,
  SESSION_INLINE_AGENT_ID,
} from '../../../server/db/migrate';

const PLATFORM_TABLES = [
  'platform_automation_invocations',
  'platform_automations',
  'platform_artifacts',
  'platform_credentials',
  'platform_environment_files',
  'platform_environment_templates',
  'platform_environments',
  'platform_events',
  'platform_input_receipts',
  'platform_items',
  'platform_sessions',
  'platform_subagents',
  'platform_traces',
  'platform_turns',
  'platform_vaults',
] as const;

const LEGACY_PLATFORM_TABLES = [
  'automation_invocations',
  'automations',
  'artifacts',
  'environment_files',
  'environment_templates',
  'environments',
  'traces',
  'vault_credentials',
  'vaults',
] as const;

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

function setup(): Database {
  const database = openDatabase(':memory:');
  db = database;
  return database;
}

function tableNames(database: Database): string[] {
  return database
    .query<{ name: string }, []>("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
    .all()
    .map((row) => row.name);
}

function insertSession(database: Database, id: string): void {
  database
    .query(
      `INSERT INTO platform_sessions (
        id, agent_id, agent_config, environment_type, status, created_at, last_active_at
      ) VALUES (?, ?, ?, 'none', 'idle', ?, ?)`,
    )
    .run(id, SESSION_INLINE_AGENT_ID, '{"instructions":""}', 1, 1);
}

function insertTurn(database: Database, id: string, sessionId: string): void {
  database
    .query(
      `INSERT INTO platform_turns (
        id, session_id, agent_id, runtime_turn_id, binding_revision, status, created_at
      ) VALUES (?, ?, ?, ?, 1, 'queued', ?)`,
    )
    .run(id, sessionId, SESSION_INLINE_AGENT_ID, `runtime-${id}`, 1);
}

function insertItem(database: Database, id: string, sessionId: string, turnId: string): void {
  database
    .query(
      `INSERT INTO platform_items (
        id, session_id, turn_id, output_index, type, status, payload, created_at
      ) VALUES (?, ?, ?, 0, 'message', 'completed', '{}', ?)`,
    )
    .run(id, sessionId, turnId, 1);
}

describe('platform schema migration', () => {
  test('creates every platform table on a fresh database', () => {
    const database = setup();

    expect(migrate(database)).toBe(3);
    expect(LATEST_SCHEMA_VERSION).toBe(3);

    const names = tableNames(database);
    for (const table of PLATFORM_TABLES) {
      expect(names).toContain(table);
      const sql = database
        .query<{ sql: string }, [string]>("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?")
        .get(table);
      expect(sql?.sql).toContain('STRICT');
    }
    for (const legacy of LEGACY_PLATFORM_TABLES) {
      expect(names).not.toContain(legacy);
    }
  });

  test('stores artifact environment and publishing turn identifiers', () => {
    const database = setup();
    migrate(database);
    insertSession(database, 'sess-1');

    database
      .query(
        `INSERT INTO platform_artifacts (
          id, session_id, environment_id, turn_id, path, size_bytes, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?)`,
      )
      .run('artifact-1', 'sess-1', 'env-1', 'turn-1', 'report.txt', 1, 1);
    database
      .query(
        `INSERT INTO platform_artifacts (id, session_id, path, size_bytes, created_at)
         VALUES (?, ?, ?, ?, ?)`,
      )
      .run('artifact-2', 'sess-1', 'local.txt', 1, 1);

    const rows = database
      .query<{ id: string; environment_id: string | null; turn_id: string | null }, []>(
        'SELECT id, environment_id, turn_id FROM platform_artifacts ORDER BY id',
      )
      .all();
    expect(rows).toEqual([
      { id: 'artifact-1', environment_id: 'env-1', turn_id: 'turn-1' },
      { id: 'artifact-2', environment_id: null, turn_id: null },
    ]);
  });

  test('creates the documented list indexes', () => {
    const database = setup();
    migrate(database);

    const indexes = new Set(
      database
        .query<{ name: string }, []>("SELECT name FROM sqlite_master WHERE type = 'index'")
        .all()
        .map((row) => row.name),
    );

    for (const index of [
      'idx_platform_sessions_created',
      'idx_platform_turns_session_created',
      'idx_platform_items_turn_output',
      'idx_platform_events_session_seq',
      'idx_platform_subagents_session_id',
      'idx_platform_automation_invocations_automation_created',
      'idx_platform_automations_status_next_run',
      'idx_platform_credentials_vault_id',
      'idx_platform_environment_files_environment_path',
    ]) {
      expect(indexes).toContain(index);
    }
  });

  test('upgrades a v2 database while preserving agents and chats', () => {
    const database = setup();

    expect(migrate(database, 2)).toBe(2);

    database
      .query(
        `INSERT INTO agents (id, name, description, config, created_at, updated_at, provider_id, revision)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        'agent-1',
        'Reviewer',
        'Checks changes',
        '{"instructions":"Be concise"}',
        1_700_000_000_000,
        1_700_000_001_000,
        null,
        2,
      );
    database
      .query(
        'INSERT INTO chats (chat_id, agent_id, session_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?)',
      )
      .run('chat-1', 'agent-1', 'session-1', 1_700_000_002_000, 1_700_000_003_000);

    expect(migrate(database)).toBe(3);
    expect(getSchemaVersion(database)).toBe(3);

    const agent = database
      .query<
        {
          name: string;
          description: string;
          config: string;
          provider_id: string | null;
          revision: number;
          created_at: number;
          model: string | null;
          metadata: string;
          tools: string;
          wire_name: string | null;
        },
        [string]
      >(
        'SELECT name, description, config, provider_id, revision, created_at, model, metadata, tools, wire_name FROM agents WHERE id = ?',
      )
      .get('agent-1');
    expect(agent).toEqual({
      name: 'Reviewer',
      description: 'Checks changes',
      config: '{"instructions":"Be concise"}',
      provider_id: null,
      revision: 2,
      created_at: 1_700_000_000_000,
      model: null,
      metadata: '{}',
      tools: '[]',
      wire_name: null,
    });

    const chat = database
      .query<{ chat_id: string; agent_id: string }, [string]>(
        'SELECT chat_id, agent_id FROM chats WHERE chat_id = ?',
      )
      .get('chat-1');
    expect(chat).toEqual({ chat_id: 'chat-1', agent_id: 'agent-1' });

    expect(
      database
        .query<{ count: number }, [string]>('SELECT COUNT(*) AS count FROM agents WHERE name = ?')
        .get('session-inline'),
    ).toEqual({ count: 1 });
  });

  test('seeds exactly one inline session agent', () => {
    const database = setup();
    migrate(database);

    const rows = database
      .query<
        {
          id: string;
          name: string;
          config: string;
          provider_id: string | null;
          revision: number;
        },
        []
      >(`SELECT id, name, config, provider_id, revision FROM agents WHERE name = 'session-inline'`)
      .all();

    expect(rows).toHaveLength(1);
    expect(rows[0]).toEqual({
      id: SESSION_INLINE_AGENT_ID,
      name: 'session-inline',
      config: '{"instructions":"","provider":null,"metaHarness":{},"mcpServers":{}}',
      provider_id: null,
      revision: 1,
    });

    // Platform agents reuse `agents.id`, so the placeholder must satisfy the public UUID contract.
    expect(z.uuid().safeParse(SESSION_INLINE_AGENT_ID).success).toBe(true);

    // Inline sessions point chats at the placeholder instead of creating an agent per session.
    database
      .query('INSERT INTO chats (chat_id, agent_id, created_at, updated_at) VALUES (?, ?, ?, ?)')
      .run('chat-inline', SESSION_INLINE_AGENT_ID, 1, 1);
    expect(
      database
        .query<{ agent_id: string }, [string]>('SELECT agent_id FROM chats WHERE chat_id = ?')
        .get('chat-inline'),
    ).toEqual({ agent_id: SESSION_INLINE_AGENT_ID });
  });

  test('requires a known agent and keeps session status inside the documented set', () => {
    const database = setup();
    migrate(database);

    insertSession(database, 'sess-known-agent');
    expect(() =>
      database
        .query(
          `INSERT INTO platform_sessions (
            id, agent_id, agent_config, environment_type, status, created_at, last_active_at
          ) VALUES ('sess-unknown', '00000000-0000-7000-8000-0000000000ff', '{}', 'none', 'idle', 1, 1)`,
        )
        .run(),
    ).toThrow();

    expect(() =>
      database
        .query(
          `INSERT INTO platform_sessions (
            id, agent_id, agent_config, environment_type, status, created_at, last_active_at
          ) VALUES ('sess-bad-status', ?, '{}', 'none', 'created', 1, 1)`,
        )
        .run(SESSION_INLINE_AGENT_ID),
    ).toThrow();
    expect(() =>
      database
        .query(
          `INSERT INTO platform_sessions (
            id, agent_id, agent_config, environment_type, status, created_at, last_active_at
          ) VALUES ('sess-bad-env', ?, '{}', 'host', 'idle', 1, 1)`,
        )
        .run(SESSION_INLINE_AGENT_ID),
    ).toThrow();
  });

  test('cascades session-owned rows on delete', () => {
    const database = setup();
    migrate(database);

    insertSession(database, 'sess-1');
    insertTurn(database, 'turn-1', 'sess-1');
    insertItem(database, 'item-1', 'sess-1', 'turn-1');
    database
      .query(
        `INSERT INTO platform_events (event_id, session_id, type, turn_id, item_id, data, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)`,
      )
      .run('event-1', 'sess-1', 'item.completed', 'turn-1', 'item-1', '{}', 1);
    database
      .query(
        `INSERT INTO platform_subagents (
          id, session_id, name, parent_agent_id, status, opened_at
        ) VALUES (?, ?, ?, ?, 'active', ?)`,
      )
      .run('subagent-1', 'sess-1', 'researcher', SESSION_INLINE_AGENT_ID, 1);
    database
      .query(
        `INSERT INTO platform_input_receipts (
          session_id, idempotency_key, digest, request_id, created_at
        ) VALUES (?, ?, ?, ?, ?)`,
      )
      .run('sess-1', 'key-1', 'digest', 'request-1', 1);
    database
      .query(
        `INSERT INTO platform_artifacts (id, session_id, path, size_bytes, created_at)
         VALUES (?, ?, ?, ?, ?)`,
      )
      .run('artifact-1', 'sess-1', 'report.txt', 1, 1);
    database
      .query(
        `INSERT INTO platform_traces (id, session_id, kind, data, created_at)
         VALUES (?, ?, ?, ?, ?)`,
      )
      .run('trace-1', 'sess-1', 'model_call', '{}', 1);

    database.query('DELETE FROM platform_sessions WHERE id = ?').run('sess-1');

    expect(database.query('SELECT id FROM platform_turns').all()).toEqual([]);
    expect(database.query('SELECT id FROM platform_items').all()).toEqual([]);
    expect(database.query('SELECT seq FROM platform_events').all()).toEqual([]);
    expect(database.query('SELECT id FROM platform_subagents').all()).toEqual([]);
    expect(database.query('SELECT session_id FROM platform_input_receipts').all()).toEqual([]);
    expect(database.query('SELECT id FROM platform_artifacts').all()).toEqual([]);
    expect(database.query('SELECT id FROM platform_traces').all()).toEqual([]);
  });

  test('cascades items when their turn is deleted', () => {
    const database = setup();
    migrate(database);

    insertSession(database, 'sess-1');
    insertTurn(database, 'turn-1', 'sess-1');
    insertItem(database, 'item-1', 'sess-1', 'turn-1');

    database.query('DELETE FROM platform_turns WHERE id = ?').run('turn-1');
    expect(database.query('SELECT id FROM platform_items').all()).toEqual([]);
  });

  test('enforces item output positions and unique event ids per turn and session', () => {
    const database = setup();
    migrate(database);

    insertSession(database, 'sess-1');
    insertTurn(database, 'turn-1', 'sess-1');
    insertItem(database, 'item-1', 'sess-1', 'turn-1');

    expect(() =>
      database
        .query(
          `INSERT INTO platform_items (
            id, session_id, turn_id, output_index, type, status, payload, created_at
          ) VALUES ('item-2', 'sess-1', 'turn-1', 0, 'message', 'completed', '{}', 1)`,
        )
        .run(),
    ).toThrow();

    database
      .query(
        `INSERT INTO platform_events (event_id, session_id, type, data, created_at)
         VALUES (?, ?, ?, ?, ?)`,
      )
      .run('event-1', 'sess-1', 'session.created', '{}', 1);
    expect(() =>
      database
        .query(
          `INSERT INTO platform_events (event_id, session_id, type, data, created_at)
           VALUES (?, ?, ?, ?, ?)`,
        )
        .run('event-1', 'sess-1', 'session.created', '{}', 2),
    ).toThrow();
  });
});
