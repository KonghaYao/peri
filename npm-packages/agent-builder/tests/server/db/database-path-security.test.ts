import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { randomBytes, randomUUID } from 'node:crypto';
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDatabase, secureDatabaseFiles } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { loadEnv } from '../../../server/env';

const roots: string[] = [];
const databases: Database[] = [];

afterEach(() => {
  for (const db of databases.splice(0)) db.close();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

/** Canonical fixture root: realpath removes the host's /var alias without depending on it. */
function canonicalRoot(): string {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-db-path-')));
  chmodSync(root, 0o700);
  roots.push(root);
  return root;
}

function directory(parent: string, name: string, mode = 0o700): string {
  const target = path.join(parent, name);
  mkdirSync(target, { mode });
  chmodSync(target, mode);
  return target;
}

function open(file: string): Database {
  const db = openDatabase(file);
  databases.push(db);
  return db;
}

function close(db: Database): void {
  const index = databases.indexOf(db);
  if (index !== -1) databases.splice(index, 1);
  db.close();
}

function source(databasePath: string, workspacePath: string, fixtureRoot: string) {
  return {
    DATABASE_PATH: databasePath,
    PERI_WORKSPACE_PATH: workspacePath,
    PERI_MACHINE_ID: '00000000-0000-4000-8000-000000000001',
    PERI_SANDBOX_PERI_PATH: path.join(fixtureRoot, 'no-peri'),
    PERI_SANDBOX_ENABLED: 'false',
    PERI_STORE_URL: 'http://127.0.0.1:9/unused',
    PERI_STORE_TOKEN: '',
    ANTHROPIC_API_KEY: '',
    OPENAI_API_KEY: '',
  };
}

function modeOf(target: string): string {
  const entry = lstatSync(target, { throwIfNoEntry: false });
  if (entry === undefined) return 'absent';
  return entry.isSymbolicLink() ? 'symlink' : (entry.mode & 0o777).toString(8);
}

/** Records entries and modes without following symlinks, so a writer shortcut is visible. */
function snapshot(directoryPath: string): string[] {
  const entries: string[] = [];
  const walk = (current: string) => {
    for (const entry of readdirSync(current, { withFileTypes: true }).sort((left, right) =>
      left.name.localeCompare(right.name),
    )) {
      const full = path.join(current, entry.name);
      const stat = lstatSync(full);
      const kind = stat.isSymbolicLink() ? 'symlink' : stat.isDirectory() ? 'dir' : 'file';
      entries.push(`${path.relative(directoryPath, full)}:${kind}:${(stat.mode & 0o777).toString(8)}`);
      if (kind === 'dir') walk(full);
    }
  };
  walk(directoryPath);
  return entries;
}

describe('database path symlink ancestors', () => {
  test('rejects the original loadEnv -> openDatabase -> migrate probe before changing the alias target', () => {
    const root = canonicalRoot();
    const workspace = directory(root, 'workspace');
    const real = directory(root, 'real');
    const privateDir = directory(real, 'private');
    const alias = path.join(root, 'alias');
    symlinkSync(real, alias);

    const databasePath = path.join(alias, 'private', 'config.db');
    const before = snapshot(real);
    const modesBefore = [modeOf(real), modeOf(privateDir)];
    let db: Database | undefined;
    let error: unknown;

    try {
      const env = loadEnv(source(databasePath, workspace, root));
      db = openDatabase(env.databasePath);
      migrate(db);
    } catch (cause) {
      error = cause;
    }

    try {
      expect(error).toBeDefined();
      expect(existsSync(path.join(real, 'private', 'config.db'))).toBe(false);
      expect(snapshot(real)).toEqual(before);
      expect([modeOf(real), modeOf(privateDir)]).toEqual(modesBefore);
      expect(lstatSync(alias).isSymbolicLink()).toBe(true);
    } finally {
      if (db !== undefined) close(db);
    }
  });

  test('rejects a multi-level alias before creating a missing last-level parent', () => {
    const root = canonicalRoot();
    const workspace = directory(root, 'workspace');
    const real = directory(root, 'real');
    const first = path.join(root, 'first');
    const second = path.join(root, 'second');
    symlinkSync(real, first);
    symlinkSync(first, second);

    const databasePath = path.join(second, 'new-parent', 'nested', 'config.db');
    const before = snapshot(real);
    let db: Database | undefined;
    let error: unknown;

    try {
      const env = loadEnv(source(databasePath, workspace, root));
      db = openDatabase(env.databasePath);
      migrate(db);
    } catch (cause) {
      error = cause;
    }

    try {
      expect(error).toBeDefined();
      expect(existsSync(path.join(real, 'new-parent'))).toBe(false);
      expect(snapshot(real)).toEqual(before);
    } finally {
      if (db !== undefined) close(db);
    }
  });

  test('rejects a symlinked final database directory without writing through it', () => {
    const root = canonicalRoot();
    const real = directory(root, 'real');
    const privateDir = directory(real, 'private');
    const linkedDirectory = path.join(root, 'private-link');
    symlinkSync(privateDir, linkedDirectory);

    const before = snapshot(real);
    expect(() => openDatabase(path.join(linkedDirectory, 'config.db'))).toThrow();
    expect(existsSync(path.join(privateDir, 'config.db'))).toBe(false);
    expect(snapshot(real)).toEqual(before);
  });

  test('rejects symlink and dangling symlink DB, WAL and SHM before chmod or open', () => {
    const root = canonicalRoot();
    const data = directory(root, 'data');
    const outside = path.join(root, 'outside.bin');
    writeFileSync(outside, 'outside', { mode: 0o644 });
    chmodSync(outside, 0o644);

    const linkedDatabase = path.join(data, 'linked.db');
    symlinkSync(outside, linkedDatabase);
    expect(() => openDatabase(linkedDatabase)).toThrow();
    expect(modeOf(outside)).toBe('644');

    const missingDatabase = path.join(root, 'missing-database.db');
    const danglingDatabase = path.join(data, 'dangling.db');
    symlinkSync(missingDatabase, danglingDatabase);
    expect(() => openDatabase(danglingDatabase)).toThrow();
    expect(existsSync(missingDatabase)).toBe(false);

    const sidecarDatabase = path.join(data, 'sidecar.db');
    writeFileSync(sidecarDatabase, '', { mode: 0o600 });

    const missingWal = path.join(root, 'missing-wal');
    symlinkSync(missingWal, `${sidecarDatabase}-wal`);
    expect(() => secureDatabaseFiles(sidecarDatabase)).toThrow();
    expect(existsSync(missingWal)).toBe(false);
    rmSync(`${sidecarDatabase}-wal`);

    const missingShm = path.join(root, 'missing-shm');
    symlinkSync(missingShm, `${sidecarDatabase}-shm`);
    expect(() => openDatabase(sidecarDatabase)).toThrow();
    expect(existsSync(missingShm)).toBe(false);

    const existingWal = path.join(root, 'existing-wal');
    writeFileSync(existingWal, 'wal', { mode: 0o644 });
    chmodSync(existingWal, 0o644);
    const linkedWalDatabase = path.join(data, 'linked-wal.db');
    writeFileSync(linkedWalDatabase, '', { mode: 0o600 });
    symlinkSync(existingWal, `${linkedWalDatabase}-wal`);
    expect(() => secureDatabaseFiles(linkedWalDatabase)).toThrow();
    expect(modeOf(existingWal)).toBe('644');
  });
});

describe('canonical private database paths', () => {
  test('keeps migration, CRUD and 0600 DB/WAL/SHM working in an existing 0700 parent', () => {
    const root = canonicalRoot();
    const data = directory(root, 'data');
    const databasePath = path.join(data, 'agent-builder.db');
    const db = open(databasePath);
    expect(migrate(db)).toBe(2);

    const key = randomBytes(24).toString('hex');
    const id = randomUUID();
    db.query('INSERT INTO providers (id, name, type, credential_source, api_key) VALUES (?, ?, ?, ?, ?)').run(
      id,
      'Local',
      'openai',
      'local',
      key,
    );
    db.query('UPDATE providers SET name = ? WHERE id = ?').run('Renamed', id);
    expect(
      db.query<{ name: string }, [string]>('SELECT name FROM providers WHERE id = ?').get(id)?.name,
    ).toBe('Renamed');
    db.query('DELETE FROM providers WHERE id = ?').run(id);
    expect(db.query('SELECT COUNT(*) AS count FROM providers').get()).toEqual({ count: 0 });

    for (const candidate of [databasePath, `${databasePath}-wal`, `${databasePath}-shm`]) {
      expect(existsSync(candidate)).toBe(true);
      expect(modeOf(candidate)).toBe('600');
    }
    expect(modeOf(data)).toBe('700');

    db.exec('PRAGMA wal_checkpoint(TRUNCATE)');
    for (const candidate of [databasePath, `${databasePath}-wal`, `${databasePath}-shm`])
      expect(modeOf(candidate)).toBe('600');

    close(db);
    const reopened = open(databasePath);
    expect(migrate(reopened)).toBe(2);
    expect(modeOf(data)).toBe('700');
    expect(modeOf(databasePath)).toBe('600');
  });

  test('creates missing parents level by level as 0700 without chmodding a shared ancestor', () => {
    const root = canonicalRoot();
    const shared = directory(root, 'shared', 0o755);
    const databasePath = path.join(shared, 'new', 'deep', 'agent-builder.db');
    const env = loadEnv(source(databasePath, directory(root, 'workspace'), root));
    const db = open(env.databasePath);
    expect(migrate(db)).toBe(2);
    db.query('INSERT INTO providers (id, name, type, credential_source, api_key) VALUES (?, ?, ?, ?, ?)').run(
      randomUUID(),
      'Local',
      'openai',
      'local',
      randomBytes(24).toString('hex'),
    );

    expect(modeOf(shared)).toBe('755');
    expect(modeOf(path.join(shared, 'new'))).toBe('700');
    expect(modeOf(path.join(shared, 'new', 'deep'))).toBe('700');
    expect(modeOf(databasePath)).toBe('600');
    close(db);
  });
});
