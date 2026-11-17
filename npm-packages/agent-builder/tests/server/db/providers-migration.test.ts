import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';

const roots: string[] = [];
const databases: Database[] = [];
afterEach(() => {
  for (const db of databases.splice(0)) db.close();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});
function root() {
  const dir = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-provider-persistence-')));
  chmodSync(dir, 0o700);
  roots.push(dir);
  return dir;
}
function open(file = ':memory:') {
  const db = openDatabase(file);
  databases.push(db);
  return db;
}

// 从源码读取已执行 v1 原文，而不是用 v2 建库伪造升级前状态。
function v1(db: Database) {
  const source = readFileSync(new URL('../../../server/db/migrate.ts', import.meta.url), 'utf8');
  const ddl = source.match(/const MIGRATION_V1 = `([\s\S]*?)`;/)?.[1];
  if (!ddl) throw new Error('v1 DDL missing');
  db.exec(ddl);
  db.exec('PRAGMA user_version = 1');
}

describe('Provider v2 migration and private SQLite', () => {
  test('v1 upgrades without rewriting inline/null Agent or changing chats; v2 reopens idempotently', () => {
    const file = path.join(root(), 'private.db');
    const db = open(file);
    v1(db);
    const inline = JSON.stringify({
      instructions: '',
      provider: { id: 'legacy', type: 'openai', model: 'old', baseUrl: 'ftp://legacy.example' },
      metaHarness: {},
      mcpServers: {},
    });
    const empty = JSON.stringify({ provider: null });
    const insert = db.query(
      'INSERT INTO agents (id, name, config, created_at, updated_at) VALUES (?, ?, ?, 1, 1)',
    );
    insert.run('legacy', 'Legacy', inline);
    insert.run('empty', 'Empty', empty);
    const chatsSql = db
      .query<{ sql: string }, []>("SELECT sql FROM sqlite_master WHERE name = 'chats'")
      .get()?.sql;
    expect(migrate(db)).toBe(2);
    const rows = db
      .query<{ config: string; provider_id: string | null; revision: number }, []>(
        'SELECT config, provider_id, revision FROM agents ORDER BY id',
      )
      .all();
    expect(rows).toEqual([
      { config: empty, provider_id: null, revision: 1 },
      { config: inline, provider_id: null, revision: 1 },
    ]);
    expect(
      db.query<{ sql: string }, []>("SELECT sql FROM sqlite_master WHERE name = 'chats'").get()?.sql,
    ).toBe(chatsSql);
    const providerFields = db
      .query<{ name: string }, []>('PRAGMA table_info(providers)')
      .all()
      .map((field) => field.name);
    expect(providerFields).toEqual([
      'id',
      'name',
      'type',
      'base_url',
      'credential_source',
      'api_key',
      'revision',
    ]);
    const indices = db
      .query<{ name: string }, []>(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
      )
      .all()
      .map((row) => row.name);
    expect(indices).toEqual(['idx_agents_provider_id', 'idx_chats_agent_id']);
    db.close();
    databases.splice(databases.indexOf(db), 1);
    const reopened = open(file);
    expect(migrate(reopened)).toBe(2);
    expect(migrate(reopened)).toBe(2);
  });

  test('failed v2 rolls back all DDL and user_version, never leaving a partial Provider schema', () => {
    const db = open();
    v1(db);
    db.exec('CREATE INDEX idx_agents_provider_id ON agents(name)');
    expect(() => migrate(db)).toThrow();
    expect(db.query<{ user_version: number }, []>('PRAGMA user_version').get()?.user_version).toBe(1);
    expect(db.query("SELECT name FROM sqlite_master WHERE name = 'providers'").get()).toBeNull();
    expect(
      db
        .query<{ name: string }, []>('PRAGMA table_info(agents)')
        .all()
        .map((row) => row.name),
    ).not.toContain('provider_id');
    db.exec('DROP INDEX idx_agents_provider_id');
    expect(migrate(db)).toBe(2);
  });

  test('new and existing DB/WAL/SHM are 0600 in a 0700 parent, including a committed secret write and reopen', () => {
    const dir = root();
    const file = path.join(dir, 'data', 'private.db');
    const db = open(file);
    migrate(db);
    const id = crypto.randomUUID();
    const secret = crypto.randomUUID();
    db.query('INSERT INTO providers (id, name, type, credential_source, api_key) VALUES (?, ?, ?, ?, ?)').run(
      id,
      'Local',
      'openai',
      'local',
      secret,
    );
    expect(statSync(path.dirname(file)).mode & 0o777).toBe(0o700);
    for (const target of [file, `${file}-wal`, `${file}-shm`]) {
      expect(existsSync(target)).toBe(true);
      expect(statSync(target).mode & 0o777).toBe(0o600);
    }
    db.close();
    databases.splice(databases.indexOf(db), 1);
    chmodSync(file, 0o644);
    const reopened = open(file);
    expect(
      reopened.query<{ api_key: string }, [string]>('SELECT api_key FROM providers WHERE id = ?').get(id)
        ?.api_key === secret,
    ).toBe(true);
    for (const target of [file, `${file}-wal`, `${file}-shm`])
      expect(statSync(target).mode & 0o777).toBe(0o600);
  });

  test('rejects shared or symlink parent and symlink DB without chmodding the shared parent or opening data', () => {
    const dir = root();
    const shared = path.join(dir, 'shared');
    mkdirSync(shared, { mode: 0o755 });
    chmodSync(shared, 0o755);
    expect(() => openDatabase(path.join(shared, 'no.db'))).toThrow();
    expect(statSync(shared).mode & 0o777).toBe(0o755);
    expect(existsSync(path.join(shared, 'no.db'))).toBe(false);
    const alias = path.join(dir, 'alias');
    symlinkSync(shared, alias);
    expect(() => openDatabase(path.join(alias, 'no.db'))).toThrow();
    const target = path.join(dir, 'target.db');
    const db = open(target);
    migrate(db);
    const link = path.join(dir, 'link.db');
    symlinkSync(target, link);
    expect(() => openDatabase(link)).toThrow();
  });
});
