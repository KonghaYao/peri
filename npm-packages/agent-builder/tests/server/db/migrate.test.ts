import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { openDatabase } from '../../../server/db/database';
import { getSchemaVersion, LATEST_SCHEMA_VERSION, migrate } from '../../../server/db/migrate';

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

describe('migrate', () => {
  test('creates schema version 2 while preserving both original tables and the chats index', () => {
    const database = setup();

    expect(migrate(database)).toBe(2);
    expect(LATEST_SCHEMA_VERSION).toBe(2);
    expect(getSchemaVersion(database)).toBe(2);

    const tables = database
      .query<{ name: string }, []>(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('agents', 'chats') ORDER BY name",
      )
      .all()
      .map((row) => row.name);
    expect(tables).toEqual(['agents', 'chats']);

    const index = database
      .query<{ name: string }, []>(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_chats_agent_id'",
      )
      .get();
    expect(index?.name).toBe('idx_chats_agent_id');
  });

  test('is idempotent', () => {
    const database = setup();

    expect(migrate(database)).toBe(2);
    expect(migrate(database)).toBe(2);
    expect(getSchemaVersion(database)).toBe(2);
  });

  test('enables foreign keys and enforces ON DELETE RESTRICT', () => {
    const database = setup();
    migrate(database);

    const foreignKeys = database.query<{ foreign_keys: number }, []>('PRAGMA foreign_keys').get();
    expect(foreignKeys?.foreign_keys).toBe(1);

    expect(() =>
      database
        .query('INSERT INTO chats (chat_id, agent_id, created_at, updated_at) VALUES (?, ?, ?, ?)')
        .run('chat-1', 'missing-agent', 1, 1),
    ).toThrow();
  });

  test('uses STRICT tables', () => {
    const database = setup();
    migrate(database);

    expect(() =>
      database
        .query(
          'INSERT INTO agents (id, name, description, config, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)',
        )
        .run('agent-1', 'A', '', '{}', 'not-an-integer', 0),
    ).toThrow();
  });

  test('refuses a database newer than the supported schema', () => {
    const database = setup();
    database.exec('PRAGMA user_version = 3');

    expect(() => migrate(database)).toThrow(/newer than the supported version/);
  });
});
