import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { request as httpRequest } from 'node:http';
import { createRequire } from 'node:module';
import { createConnection } from 'node:net';
import { networkInterfaces, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test, { type TestContext } from 'node:test';
import { connect } from '@tursodatabase/database';
import { Client, type ClientOptions, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { verifyDatabaseFile } from '../src/database-file.js';
import { executeSql, type ExecuteResult, type TableDescription, type TableInfo } from '../src/database.js';
import { isAllowedOrigin, startServer } from '../src/server.js';

const FIXTURE_SQL = `
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL DEFAULT 'anonymous',
  active INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT);
CREATE TABLE audit (id INTEGER PRIMARY KEY, message TEXT);
CREATE TABLE big_values (id INTEGER PRIMARY KEY, big INTEGER, payload BLOB, note TEXT);
CREATE TABLE memberships (user_id INTEGER, group_id INTEGER, PRIMARY KEY (user_id, group_id));
CREATE TABLE auto_ids (id INTEGER PRIMARY KEY AUTOINCREMENT, value TEXT);
CREATE VIEW active_users AS SELECT id, name FROM users WHERE active = 1;
INSERT INTO users (id, name, active) VALUES (1, 'Alice', 1), (2, 'Bob', 0);
`;

interface ToolCallResult {
  content: Array<{ type: string; text?: string }>;
  structuredContent?: unknown;
  isError?: boolean;
}

interface Fixture {
  databasePath: string;
  port: number;
  url: string;
  connect(options?: ClientOptions): Promise<Client>;
}

function textOf(result: ToolCallResult): string {
  return result.content.map((block) => block.text ?? '').join('\n');
}

function structured<T>(result: ToolCallResult): T {
  assert.equal(result.isError, undefined);
  assert.notEqual(result.structuredContent, undefined);
  return result.structuredContent as T;
}

async function callTool(client: Client, name: string, args: Record<string, unknown> = {}): Promise<ToolCallResult> {
  return (await client.callTool({ name, arguments: args })) as unknown as ToolCallResult;
}

async function fixture(t: TestContext, sql = FIXTURE_SQL): Promise<Fixture> {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-test-'));
  const databasePath = join(directory, 'fixture.db');
  const clients: Client[] = [];
  let server: Awaited<ReturnType<typeof startServer>> | undefined;

  try {
    const database = await connect(databasePath);
    await database.exec(sql);
    await database.close();
    server = await startServer({ databasePath, port: 0 });
  } catch (error) {
    await rm(directory, { recursive: true, force: true });
    throw error;
  }

  t.after(async () => {
    for (const client of clients.reverse()) {
      await client.close().catch(() => undefined);
    }
    if (server !== undefined) await server.close().catch(() => undefined);
    await rm(directory, { recursive: true, force: true });
  });

  return {
    databasePath,
    port: server.port,
    url: server.url,
    async connect(options?: ClientOptions) {
      const client = new Client({ name: 'sqlite-mcp-test', version: '1.0.0' }, options);
      clients.push(client);
      await client.connect(new StreamableHTTPClientTransport(new URL(server.url)));
      return client;
    },
  };
}

test('legacy client discovers exactly three tools and mcp-use telemetry stays disabled', {
  timeout: 20_000,
}, async (t) => {
  assert.equal(process.env.MCP_USE_ANONYMIZED_TELEMETRY, 'false');
  const f = await fixture(t);
  const client = await f.connect();
  const tools = await client.listTools();
  assert.deepEqual(tools.tools.map((tool) => tool.name).sort(), ['describe_table', 'execute_sql', 'list_tables']);
});

test('modern 2026-07-28 client also discovers the tools', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect({ versionNegotiation: { mode: { pin: '2026-07-28' } } });
  const tools = await client.listTools();
  assert.deepEqual(tools.tools.map((tool) => tool.name).sort(), ['describe_table', 'execute_sql', 'list_tables']);
});

test('parameterized writes and normal DDL work', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const created = await callTool(client, 'execute_sql', {
    sql: 'CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)',
  });
  assert.deepEqual(structured<ExecuteResult>(created).kind, 'none');

  const inserted = await callTool(client, 'execute_sql', {
    sql: 'INSERT INTO items (value) VALUES (?)',
    params: ['first'],
  });
  const insertResult = structured<ExecuteResult>(inserted);
  assert.equal(insertResult.kind, 'none');
  assert.equal(insertResult.changes, 1);

  const selected = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', {
      sql: 'SELECT id, value FROM items WHERE value = ?',
      params: ['first'],
    }),
  );
  assert.equal(selected.kind, 'rows');
  assert.deepEqual(selected.columns, ['id', 'value']);
  assert.equal(selected.rows[0]?.[1], 'first');
  assert.equal(selected.rows.length, 1);

  const boolean = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT ? AS flag', params: [true] }),
  );
  assert.equal(boolean.rows[0]?.[0], 1);
});

test('database errors are categorized without echoing SQL or parameters', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const sqlMarker = await callTool(client, 'execute_sql', {
    sql: "SELECT 'SQL_MARKER_ABC' FROM missing_table",
  });
  assert.equal(sqlMarker.isError, true);
  assert.doesNotMatch(textOf(sqlMarker), /SQL_MARKER_ABC|SELECT/);

  const paramMarker = await callTool(client, 'execute_sql', {
    sql: 'INSERT INTO items (no_such_column) VALUES (?)',
    params: ['PARAM_MARKER_ABC'],
  });
  assert.equal(paramMarker.isError, true);
  assert.doesNotMatch(textOf(paramMarker), /PARAM_MARKER_ABC|no_such_column/);

  const validationMarker = await callTool(client, 'execute_sql', {
    sql: 'SELECT ?',
    params: [{ secret: 'PARAM_OBJECT_MARKER_ABC' }],
  });
  assert.equal(validationMarker.isError, true);
  assert.doesNotMatch(textOf(validationMarker), /PARAM_OBJECT_MARKER_ABC/);
});

test('list_tables and describe_table expose tables, views, keys and raw SQL', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const tables = structured<{ tables: TableInfo[] }>(await callTool(client, 'list_tables')).tables;
  assert.ok(tables.some((table) => table.name === 'users' && table.type === 'table'));
  assert.ok(tables.some((table) => table.name === 'active_users' && table.type === 'view'));
  assert.ok(tables.every((table) => !table.name.startsWith('sqlite_') && !table.name.startsWith('__turso_internal_')));

  const users = structured<TableDescription>(await callTool(client, 'describe_table', { table: 'users' }));
  assert.equal(users.type, 'table');
  assert.match(users.sql ?? '', /CREATE TABLE users/);
  const id = users.columns.find((column) => column.name === 'id');
  assert.equal(id?.type, 'INTEGER');
  assert.equal(id?.primaryKey, true);
  assert.equal(id?.primaryKeyPosition, 1);
  const name = users.columns.find((column) => column.name === 'name');
  assert.equal(name?.type, 'TEXT');
  assert.equal(name?.notNull, true);
  assert.equal(name?.defaultValue, "'anonymous'");

  const composite = structured<TableDescription>(await callTool(client, 'describe_table', { table: 'memberships' }));
  assert.deepEqual(
    composite.columns.filter((column) => column.primaryKey).map((column) => [column.name, column.primaryKeyPosition]),
    [
      ['user_id', 1],
      ['group_id', 2],
    ],
  );

  const view = structured<TableDescription>(await callTool(client, 'describe_table', { table: 'active_users' }));
  assert.equal(view.type, 'view');
  assert.match(view.sql ?? '', /CREATE VIEW active_users/);
  assert.deepEqual(
    view.columns.map((column) => column.name),
    ['id', 'name'],
  );
  assert.ok(view.columns.every((column) => column.primaryKey === false));
});

test('schema tools bind names rather than appending SQL', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const created = await callTool(client, 'execute_sql', {
    sql: 'CREATE TABLE "odd;name" ("odd""col" TEXT)',
  });
  assert.equal(created.isError, undefined);

  const odd = structured<TableDescription>(await callTool(client, 'describe_table', { table: 'odd;name' }));
  assert.equal(odd.columns[0]?.name, 'odd"col');

  const rejected = await callTool(client, 'describe_table', { table: 'users; DROP TABLE users' });
  assert.equal(rejected.isError, true);
  assert.doesNotMatch(textOf(rejected), /users; DROP/);

  const tables = structured<{ tables: TableInfo[] }>(await callTool(client, 'list_tables')).tables;
  assert.ok(tables.some((table) => table.name === 'users'));
});

test('multi-statement input is rejected before any prefix executes', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const write = await callTool(client, 'execute_sql', {
    sql: "INSERT INTO items (value) VALUES ('first'); INSERT INTO items (value) VALUES ('second')",
  });
  assert.equal(write.isError, true);
  assert.match(textOf(write), /一条 SQL/);

  const count = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT count(*) AS count FROM items' }),
  );
  assert.equal(count.rows[0]?.[0], 0);

  const ddl = await callTool(client, 'execute_sql', {
    sql: 'CREATE TABLE partial_one (x); CREATE TABLE partial_two (y)',
  });
  assert.equal(ddl.isError, true);
  const tables = structured<{ tables: TableInfo[] }>(await callTool(client, 'list_tables')).tables;
  assert.ok(!tables.some((table) => table.name === 'partial_one' || table.name === 'partial_two'));
});

test('semicolons in strings/comments and a single trigger body are accepted', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const stringSemicolon = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: "SELECT 'a;b' AS value -- trailing ; comment" }),
  );
  assert.equal(stringSemicolon.rows[0]?.[0], 'a;b');

  const commentSemicolon = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT 1 AS value /* ; */' }),
  );
  assert.equal(commentSemicolon.rows[0]?.[0], 1);

  const trigger = await callTool(client, 'execute_sql', {
    sql: `CREATE TRIGGER audit_items AFTER INSERT ON items BEGIN
      INSERT INTO audit (message) VALUES ('one;two');
      INSERT INTO audit (message) VALUES (CASE WHEN new.value IS NULL THEN 'null' ELSE new.value END);
    END`,
  });
  assert.equal(trigger.isError, undefined);

  await callTool(client, 'execute_sql', { sql: "INSERT INTO items (value) VALUES ('triggered')" });
  const audit = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT message FROM audit ORDER BY id' }),
  );
  assert.deepEqual(
    audit.rows.map((row) => row[0]),
    ['one;two', 'triggered'],
  );
});

test('responses truncate at 100 rows and say so without adding LIMIT', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const many = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', {
      sql: 'WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM seq WHERE x < 150) SELECT x FROM seq',
    }),
  );
  assert.equal(many.kind, 'rows');
  assert.equal(many.returnedRowCount, 100);
  assert.equal(many.rows.length, 100);
  assert.equal(many.totalRowCount, 150);
  assert.equal(many.truncated, true);
  assert.equal(many.rows[99]?.[0], 100);

  const exact = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', {
      sql: 'WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM seq WHERE x < 100) SELECT x FROM seq',
    }),
  );
  assert.equal(exact.returnedRowCount, 100);
  assert.equal(exact.totalRowCount, 100);
  assert.equal(exact.truncated, false);
});

test('RETURNING writes finish even when the response is truncated', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const returned = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', {
      sql: `WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM seq WHERE x < 150)
            INSERT INTO items (value) SELECT 'v' || x FROM seq RETURNING id, value`,
    }),
  );
  assert.equal(returned.kind, 'rows');
  assert.equal(returned.returnedRowCount, 100);
  assert.equal(returned.totalRowCount, 150);
  assert.equal(returned.truncated, true);

  const count = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT count(*) AS count FROM items' }),
  );
  assert.equal(count.rows[0]?.[0], 150);
});

test('column metadata distinguishes empty results, PRAGMA, CTE and no-result statements', {
  timeout: 20_000,
}, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const empty = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT id FROM users WHERE 0' }),
  );
  assert.equal(empty.kind, 'rows');
  assert.deepEqual(empty.columns, ['id']);
  assert.deepEqual(empty.rows, []);
  assert.equal(empty.truncated, false);

  const pragma = structured<ExecuteResult>(await callTool(client, 'execute_sql', { sql: 'PRAGMA user_version' }));
  assert.equal(pragma.kind, 'rows');
  assert.deepEqual(pragma.columns, ['user_version']);
  assert.deepEqual(pragma.rows, [[0]]);

  const noResultPragma = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'PRAGMA foreign_keys = ON' }),
  );
  assert.equal(noResultPragma.kind, 'none');
  assert.deepEqual(noResultPragma.columns, []);

  const cte = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'WITH x AS (SELECT 1 AS n) SELECT n FROM x' }),
  );
  assert.equal(cte.kind, 'rows');
  assert.equal(cte.rows[0]?.[0], 1);

  const ddl = structured<ExecuteResult>(await callTool(client, 'execute_sql', { sql: 'CREATE TABLE kind_test (x)' }));
  assert.equal(ddl.kind, 'none');
  assert.equal(ddl.changes, 0);
});

test('driver defaults keep ATTACH and extension loading disabled', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();
  const attachedPath = join(dirname(f.databasePath), 'attached.db');

  const attach = await callTool(client, 'execute_sql', {
    sql: 'ATTACH DATABASE ? AS other',
    params: [attachedPath],
  });
  assert.equal(attach.isError, true);
  await assert.rejects(stat(attachedPath));

  const extension = await callTool(client, 'execute_sql', { sql: "SELECT load_extension('x')" });
  assert.equal(extension.isError, true);
});

test('a database deleted after startup is not recreated by a tool call', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();
  await rm(f.databasePath, { force: true });

  const result = await callTool(client, 'execute_sql', { sql: 'SELECT 1' });
  assert.equal(result.isError, true);
  await assert.rejects(stat(f.databasePath));
});

test('reads and writes a standard delete-journal SQLite file', { timeout: 20_000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-standard-'));
  const databasePath = join(directory, 'standard.db');
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });

  interface SqliteSync {
    exec(sql: string): void;
    prepare(sql: string): { get(): Record<string, unknown> };
    close(): void;
  }
  let DatabaseSync: (new (path: string) => SqliteSync) | undefined;
  try {
    const sqliteModule = createRequire(import.meta.url)('node:sqlite') as {
      DatabaseSync?: new (path: string) => SqliteSync;
    };
    DatabaseSync = sqliteModule.DatabaseSync;
  } catch {
    DatabaseSync = undefined;
  }
  if (DatabaseSync === undefined) {
    t.skip('node:sqlite is unavailable');
    return;
  }

  const sqlite = new DatabaseSync(databasePath);
  sqlite.exec(`
    CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, big INTEGER, payload BLOB);
    CREATE VIEW user_names AS SELECT name FROM users;
    INSERT INTO users (name, big, payload) VALUES ('standard', 9007199254740993, x'0102');
  `);
  const journalMode = sqlite.prepare('PRAGMA journal_mode').get();
  assert.equal(journalMode.journal_mode, 'delete');
  sqlite.close();
  await verifyDatabaseFile(databasePath);
  const walPath = `${databasePath}-wal`;
  await assert.rejects(stat(walPath));

  const server = await startServer({ databasePath, port: 0 });
  t.after(async () => {
    await server.close();
  });
  const client = new Client({ name: 'sqlite-mcp-standard-test', version: '1.0.0' });
  t.after(async () => {
    await client.close().catch(() => undefined);
  });
  await client.connect(new StreamableHTTPClientTransport(new URL(server.url)));

  const listed = structured<{ tables: TableInfo[] }>(await callTool(client, 'list_tables')).tables;
  assert.ok(listed.some((table) => table.name === 'user_names' && table.type === 'view'));
  const described = structured<TableDescription>(await callTool(client, 'describe_table', { table: 'users' }));
  assert.equal(described.type, 'table');
  await assert.rejects(stat(walPath));

  const before = await stat(databasePath, { bigint: true });
  const read = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT big, payload FROM users' }),
  );
  assert.deepEqual(read.rows[0], [
    { type: 'integer', value: '9007199254740993' },
    { type: 'blob', base64: 'AQI=', byteLength: 2 },
  ]);
  const after = await stat(databasePath, { bigint: true });
  assert.equal(after.dev, before.dev);
  assert.equal(after.ino, before.ino);
  assert.equal((await stat(walPath)).isFile(), true);

  const write = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: `INSERT INTO users (name) VALUES ('second')` }),
  );
  assert.equal(write.kind, 'none');
  assert.equal(write.kind === 'none' ? write.changes : undefined, 1);

  const count = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT count(*) AS count FROM users' }),
  );
  assert.equal(count.rows[0]?.[0], 2);
});

test('strict open recreates a missing WAL sidecar for an existing WAL database', { timeout: 20_000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-wal-'));
  const databasePath = join(directory, 'wal.db');
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });

  interface SqliteSync {
    exec(sql: string): void;
    prepare(sql: string): { get(): Record<string, unknown> };
    close(): void;
  }
  let DatabaseSync: (new (path: string) => SqliteSync) | undefined;
  try {
    const sqliteModule = createRequire(import.meta.url)('node:sqlite') as {
      DatabaseSync?: new (path: string) => SqliteSync;
    };
    DatabaseSync = sqliteModule.DatabaseSync;
  } catch {
    DatabaseSync = undefined;
  }
  if (DatabaseSync === undefined) {
    t.skip('node:sqlite is unavailable');
    return;
  }

  const sqlite = new DatabaseSync(databasePath);
  sqlite.exec(`
    PRAGMA journal_mode = wal;
    CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
    INSERT INTO users (name) VALUES ('wal');
    PRAGMA wal_checkpoint(TRUNCATE);
  `);
  const journalMode = sqlite.prepare('PRAGMA journal_mode').get();
  assert.equal(journalMode.journal_mode, 'wal');
  sqlite.close();
  await rm(`${databasePath}-wal`, { force: true });
  await rm(`${databasePath}-shm`, { force: true });
  await assert.rejects(stat(`${databasePath}-wal`));

  const before = await stat(databasePath, { bigint: true });
  const read = await executeSql(databasePath, 'SELECT id, name FROM users');
  assert.equal(read.kind, 'rows');
  assert.deepEqual(read.rows[0], [1, 'wal']);

  const write = await executeSql(databasePath, `INSERT INTO users (name) VALUES ('second')`);
  assert.equal(write.kind, 'none');
  assert.equal(write.kind === 'none' ? write.changes : undefined, 1);
  assert.equal((await stat(`${databasePath}-wal`)).isFile(), true);
  const after = await stat(databasePath, { bigint: true });
  assert.equal(after.dev, before.dev);
  assert.equal(after.ino, before.ino);
});

test('strict open never creates a missing main file, even with a WAL sidecar', { timeout: 20_000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-strict-'));
  const databasePath = join(directory, 'missing.db');
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });
  await writeFile(`${databasePath}-wal`, '');

  await assert.rejects(executeSql(databasePath, 'SELECT 1'));
  await assert.rejects(stat(databasePath));
});

test('failed strict open cleans up only its own empty WAL sidecar', { timeout: 20_000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-cleanup-'));
  const databasePath = join(directory, 'invalid.db');
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });
  await writeFile(databasePath, 'NOT_A_SQLITE_DATABASE');

  await assert.rejects(executeSql(databasePath, 'SELECT 1'));
  assert.equal(await readFile(databasePath, 'utf8'), 'NOT_A_SQLITE_DATABASE');
  await assert.rejects(stat(`${databasePath}-wal`));
});

test('failed strict open neither overwrites nor removes a pre-existing WAL sidecar', { timeout: 20_000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-owned-'));
  const databasePath = join(directory, 'invalid.db');
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });
  await writeFile(databasePath, 'NOT_A_SQLITE_DATABASE');
  await writeFile(`${databasePath}-wal`, 'SENTINEL');

  await assert.rejects(executeSql(databasePath, 'SELECT 1'));
  assert.equal(await readFile(databasePath, 'utf8'), 'NOT_A_SQLITE_DATABASE');
  assert.equal(await readFile(`${databasePath}-wal`, 'utf8'), 'SENTINEL');
});

test('VACUUM INTO is blocked before execution while EXPLAIN stays read-only', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();
  const target = join(dirname(f.databasePath), 'export.db');
  const sqlTarget = target.replaceAll("'", "''");

  const blocked = await callTool(client, 'execute_sql', { sql: `VACUUM INTO '${sqlTarget}'` });
  assert.equal(blocked.isError, true);
  assert.match(textOf(blocked), /VACUUM INTO/);
  assert.ok(!textOf(blocked).includes(target));
  await assert.rejects(stat(target));

  const plan = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: `EXPLAIN VACUUM INTO '${sqlTarget}'` }),
  );
  assert.equal(plan.kind, 'rows');
  assert.ok(plan.rows.some((row) => row[1] === 'VacuumInto'));
  await assert.rejects(stat(target));
});

test('single trigger with a Unicode END-prefixed identifier executes', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  await callTool(client, 'execute_sql', { sql: 'CREATE TABLE source ("END标记" TEXT)' });
  await callTool(client, 'execute_sql', { sql: `INSERT INTO source ("END标记") VALUES ('unicode')` });
  const trigger = await callTool(client, 'execute_sql', {
    sql: `CREATE TRIGGER source_audit AFTER INSERT ON items BEGIN
      INSERT INTO audit (message) SELECT END标记 FROM source;
    END`,
  });
  assert.equal(trigger.isError, undefined);

  await callTool(client, 'execute_sql', { sql: "INSERT INTO items (value) VALUES ('triggered')" });
  const audit = structured<ExecuteResult>(await callTool(client, 'execute_sql', { sql: 'SELECT message FROM audit' }));
  assert.deepEqual(
    audit.rows.map((row) => row[0]),
    ['unicode'],
  );
});

test('large integers and blobs are represented safely', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  await callTool(client, 'execute_sql', {
    sql: "INSERT INTO big_values (big, payload, note) VALUES (9007199254740993, x'0102', NULL)",
  });
  const result = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT big, payload, note, 42 AS small FROM big_values' }),
  );
  assert.deepEqual(result.rows[0], [
    { type: 'integer', value: '9007199254740993' },
    { type: 'blob', base64: 'AQI=', byteLength: 2 },
    null,
    42,
  ]);
});

test('constraint failure leaves no partial write', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  await callTool(client, 'execute_sql', { sql: 'CREATE TABLE unique_items (value TEXT UNIQUE)' });
  const first = await callTool(client, 'execute_sql', {
    sql: "INSERT INTO unique_items (value) VALUES ('same')",
  });
  assert.equal(first.isError, undefined);

  const duplicate = await callTool(client, 'execute_sql', {
    sql: "INSERT INTO unique_items (value) VALUES ('same')",
  });
  assert.equal(duplicate.isError, true);
  const count = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: 'SELECT count(*) AS count FROM unique_items' }),
  );
  assert.equal(count.rows[0]?.[0], 1);
});

test('transactions do not survive between tool calls', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const client = await f.connect();

  const begin = structured<ExecuteResult>(await callTool(client, 'execute_sql', { sql: 'BEGIN' }));
  assert.equal(begin.kind, 'none');

  const insert = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: "INSERT INTO items (value) VALUES ('after-begin')" }),
  );
  assert.equal(insert.kind, 'none');
  assert.equal(insert.kind === 'none' ? insert.changes : undefined, 1);

  const commit = await callTool(client, 'execute_sql', { sql: 'COMMIT' });
  assert.equal(commit.isError, true);

  const rows = structured<ExecuteResult>(
    await callTool(client, 'execute_sql', { sql: "SELECT value FROM items WHERE value = 'after-begin'" }),
  );
  assert.equal(rows.rows.length, 1);
});

test('external Host and Origin are rejected; / serves instructions and only /mcp is routed', {
  timeout: 20_000,
}, async (t) => {
  assert.equal(isAllowedOrigin(undefined), true);
  assert.equal(isAllowedOrigin(''), false);
  const f = await fixture(t);
  const host = `127.0.0.1:${f.port}`;
  const initBody = JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    method: 'initialize',
    params: {
      protocolVersion: '2025-06-18',
      capabilities: {},
      clientInfo: { name: 'raw-http-test', version: '1.0.0' },
    },
  });
  const rpcHeaders = {
    host,
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
    'mcp-protocol-version': '2025-06-18',
  };

  const allowed = await rawRequest(f.port, '/mcp', 'POST', rpcHeaders, initBody);
  assert.equal(allowed.status, 200, allowed.body);
  assert.equal(allowed.headers['access-control-allow-origin'], undefined);

  const localOrigin = await rawRequest(
    f.port,
    '/mcp',
    'POST',
    { ...rpcHeaders, origin: `http://127.0.0.1:${f.port}` },
    initBody,
  );
  assert.equal(localOrigin.status, 200, localOrigin.body);

  const localhostHost = await rawRequest(
    f.port,
    '/mcp',
    'POST',
    { ...rpcHeaders, host: `localhost:${f.port}` },
    initBody,
  );
  assert.equal(localhostHost.status, 200, localhostHost.body);

  const badHost = await rawRequest(f.port, '/mcp', 'POST', { ...rpcHeaders, host: 'evil.example' }, initBody);
  assert.equal(badHost.status, 403);

  const spoofedHost = await rawRequest(
    f.port,
    '/mcp',
    'POST',
    { ...rpcHeaders, host: `127.0.0.1.evil.example` },
    initBody,
  );
  assert.equal(spoofedHost.status, 403);

  const userinfoHost = await rawRequest(
    f.port,
    '/mcp',
    'POST',
    { ...rpcHeaders, host: `evil.example@127.0.0.1:${f.port}` },
    initBody,
  );
  assert.equal(userinfoHost.status, 403);

  const fragmentHost = await rawRequest(
    f.port,
    '/mcp',
    'POST',
    { ...rpcHeaders, host: `127.0.0.1:${f.port}#fragment` },
    initBody,
  );
  assert.equal(fragmentHost.status, 403);

  for (const origin of [
    'http://evil.example',
    'null',
    'ftp://127.0.0.1',
    'http://127.0.0.1@evil.example',
    `http://127.0.0.1:${f.port}/path`,
    `http://user@127.0.0.1:${f.port}`,
    '',
  ]) {
    const response = await rawRequest(f.port, '/mcp', 'POST', { ...rpcHeaders, origin }, initBody);
    assert.equal(response.status, 403, `origin should be rejected: ${origin}`);
    assert.equal(response.headers['access-control-allow-origin'], undefined);
  }

  const externalGet = await rawRequest(f.port, '/mcp', 'GET', {
    host,
    origin: 'http://evil.example',
    accept: 'text/event-stream',
  });
  assert.equal(externalGet.status, 403);

  const html = await rawRequest(f.port, '/mcp', 'GET', { host, accept: 'text/html' });
  assert.equal(html.status, 404);
  assert.doesNotMatch(html.body, /<!DOCTYPE html>/i);

  const instructions = await rawRequest(f.port, '/', 'GET', { host });
  assert.equal(instructions.status, 200);
  assert.equal(instructions.headers['content-type'], 'text/html; charset=utf-8');
  assert.match(instructions.body, /<!DOCTYPE html>/i);
  assert.match(instructions.body, /Streamable HTTP/);
  assert.match(instructions.body, new RegExp(`http://127\\.0\\.0\\.1:${f.port}/mcp`));
  for (const name of ['list_tables', 'describe_table', 'execute_sql']) {
    assert.match(instructions.body, new RegExp(name));
  }
  assert.match(instructions.body, /"table":"example_table"/);
  assert.match(instructions.body, /"sql":"SELECT \* FROM example_table LIMIT 5"/);
  assert.match(instructions.body, /"params":\[\]/);
  assert.match(instructions.body, /无鉴权/);
  assert.match(instructions.body, /写权限/);
  assert.match(instructions.body, /串行/);
  assert.doesNotMatch(instructions.body, /<script|<form|<link|<img/i);

  const head = await rawRequest(f.port, '/', 'HEAD', { host });
  assert.equal(head.status, 200);
  assert.equal(head.headers['content-type'], 'text/html; charset=utf-8');
  assert.equal(head.body, '');

  const rootBadHost = await rawRequest(f.port, '/', 'GET', { host: 'evil.example' });
  assert.equal(rootBadHost.status, 403);
  assert.doesNotMatch(rootBadHost.body, /<!DOCTYPE html>/i);
  const rootBadOrigin = await rawRequest(f.port, '/', 'GET', { host, origin: 'http://evil.example' });
  assert.equal(rootBadOrigin.status, 403);
  assert.doesNotMatch(rootBadOrigin.body, /<!DOCTYPE html>/i);

  for (const path of ['/health', '/favicon.ico', '/mcp/', '/mcp/_mcp-use/public/app.js']) {
    const response = await rawRequest(f.port, path, 'GET', { host });
    assert.equal(response.status, 404, `path should not be exposed: ${path}`);
    assert.equal(response.headers['access-control-allow-origin'], undefined);
  }

  const preflight = await rawRequest(f.port, '/mcp', 'OPTIONS', {
    host,
    origin: 'http://evil.example',
    'access-control-request-method': 'POST',
  });
  assert.equal(preflight.status, 403);
  assert.equal(preflight.headers['access-control-allow-origin'], undefined);
});

test('the HTTP listener is loopback-only', { timeout: 20_000 }, async (t) => {
  const f = await fixture(t);
  const address = Object.values(networkInterfaces())
    .flat()
    .find((info) => info !== undefined && info.family === 'IPv4' && !info.internal)?.address;
  if (address === undefined) {
    t.skip('no non-loopback IPv4 interface');
    return;
  }
  assert.equal(await canConnect(address, f.port), false);
});

interface RawResponse {
  status: number | undefined;
  headers: Record<string, string | string[] | undefined>;
  body: string;
}

function rawRequest(
  port: number,
  path: string,
  method: string,
  headers: Record<string, string>,
  body?: string,
): Promise<RawResponse> {
  return new Promise((resolve, reject) => {
    const req = httpRequest(
      { host: '127.0.0.1', port, path, method, agent: false, headers: { connection: 'close', ...headers } },
      (res) => {
        let data = '';
        res.setEncoding('utf8');
        res.on('data', (chunk) => {
          data += chunk;
        });
        res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, body: data }));
      },
    );
    req.once('error', reject);
    if (body !== undefined) req.write(body);
    req.end();
  });
}

function canConnect(host: string, port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = createConnection({ host, port });
    const finish = (value: boolean) => {
      socket.destroy();
      resolve(value);
    };
    socket.once('connect', () => finish(true));
    socket.once('error', () => finish(false));
    socket.setTimeout(750, () => finish(false));
  });
}
