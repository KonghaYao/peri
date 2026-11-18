import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test, { type TestContext } from 'node:test';
import { connect } from '@tursodatabase/database';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';

const CLI = fileURLToPath(new URL('../src/cli.js', import.meta.url));

async function temporaryDatabase(t: TestContext, sql: string): Promise<{ directory: string; databasePath: string }> {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-cli-'));
  const databasePath = join(directory, 'fixture.db');
  const database = await connect(databasePath);
  await database.exec(sql);
  await database.close();
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });
  return { directory, databasePath };
}

interface ChildServer {
  child: ChildProcess;
  url: string;
  stdout(): string;
  stderr(): string;
  stop(): Promise<void>;
}

/** 用 HOST/PORT 环境变量制造错误默认值的诱饵，CLI 必须仍然显式绑定 127.0.0.1。 */
async function startChild(t: TestContext, databasePath: string): Promise<ChildServer> {
  const child = spawn(process.execPath, [CLI, databasePath, '--port', '0'], {
    env: { ...process.env, HOST: '0.0.0.0', PORT: '1', MCP_USE_LOG_LEVEL: 'trace' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stdout?.setEncoding('utf8');
  child.stderr?.setEncoding('utf8');
  child.stdout?.on('data', (chunk: string) => {
    stdout += chunk;
  });
  child.stderr?.on('data', (chunk: string) => {
    stderr += chunk;
  });
  t.after(async () => {
    await stopChild(child);
  });

  const url = await new Promise<string>((resolve, reject) => {
    const startedAt = Date.now();
    const timer = setInterval(() => {
      const match = stdout.match(/http:\/\/127\.0\.0\.1:(\d+)\/mcp/);
      if (match?.[0] !== undefined) {
        clearInterval(timer);
        resolve(match[0]);
      } else if (child.exitCode !== null) {
        clearInterval(timer);
        reject(new Error(`server exited with ${child.exitCode}: ${stderr}`));
      } else if (Date.now() - startedAt > 10_000) {
        clearInterval(timer);
        reject(new Error(`server did not start: ${stderr}`));
      }
    }, 20);
  });

  return { child, url, stdout: () => stdout, stderr: () => stderr, stop: () => stopChild(child) };
}

async function stopChild(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null) return;
  child.kill('SIGTERM');
  const timer = setTimeout(() => {
    child.kill('SIGKILL');
  }, 3_000);
  await once(child, 'exit').catch(() => undefined);
  clearTimeout(timer);
}

async function runChild(args: readonly string[]): Promise<{ code: number | null; stdout: string; stderr: string }> {
  const child = spawn(process.execPath, [CLI, ...args], { stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  child.stdout?.setEncoding('utf8');
  child.stderr?.setEncoding('utf8');
  child.stdout?.on('data', (chunk: string) => {
    stdout += chunk;
  });
  child.stderr?.on('data', (chunk: string) => {
    stderr += chunk;
  });
  const timer = setTimeout(() => {
    child.kill('SIGKILL');
  }, 15_000);
  try {
    const [code] = (await once(child, 'exit')) as [number | null];
    return { code, stdout, stderr };
  } finally {
    clearTimeout(timer);
  }
}

test('--help exits zero and documents the CLI contract', async () => {
  const result = await runChild(['--help']);
  assert.equal(result.code, 0);
  assert.match(result.stdout, /sqlite-mcp <数据库路径> \[--port <端口>\]/);
  assert.match(result.stdout, /127\.0\.0\.1/);
  assert.match(result.stdout, /WAL/);
  assert.match(result.stdout, /侧车/);
  assert.match(result.stdout, /并发/);
  assert.match(result.stdout, /可信本机/);
  assert.equal(result.stderr, '');
});

test('a missing database path exits non-zero without creating the target', async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-missing-'));
  const databasePath = join(directory, 'missing.db');
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });

  const result = await runChild([databasePath, '--port', '0']);
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /不存在/);
  await assert.rejects(stat(databasePath));
  await assert.rejects(stat(`${databasePath}-wal`));
});

test('a directory path exits non-zero', async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-dir-'));
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });

  const result = await runChild([directory, '--port', '0']);
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /不是普通文件/);
});

test('invalid ports are rejected before startup', async () => {
  const result = await runChild(['ignored.db', '--port', '70000']);
  assert.equal(result.code, 2);
  assert.match(result.stderr, /端口/);
  assert.match(result.stderr, /用法/);
});

test('a non-SQLite file fails startup and is left unchanged', async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'sqlite-mcp-badfile-'));
  const databasePath = join(directory, 'bad.db');
  t.after(async () => {
    await rm(directory, { recursive: true, force: true });
  });
  await writeFile(databasePath, 'NOT_A_SQLITE_DATABASE');

  const result = await runChild([databasePath, '--port', '0']);
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /无法作为 SQLite 数据库打开/);
  assert.equal(await readFile(databasePath, 'utf8'), 'NOT_A_SQLITE_DATABASE');
  await assert.rejects(stat(`${databasePath}-wal`));
});

test('the installed-style Node CLI serves MCP without logging SQL, params or rows', { timeout: 30_000 }, async (t) => {
  const { databasePath } = await temporaryDatabase(
    t,
    `CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT);
     CREATE VIEW item_values AS SELECT value FROM items;
     INSERT INTO items (value) VALUES ('ROW_MARKER_ABC');`,
  );
  const server = await startChild(t, databasePath);
  const client = new Client({ name: 'sqlite-mcp-cli-test', version: '1.0.0' });
  t.after(async () => {
    await client.close().catch(() => undefined);
  });
  await client.connect(new StreamableHTTPClientTransport(new URL(server.url)));

  const tools = await client.listTools();
  assert.deepEqual(tools.tools.map((tool) => tool.name).sort(), ['describe_table', 'execute_sql', 'list_tables']);

  const failed = (await client.callTool({
    name: 'execute_sql',
    arguments: { sql: "SELECT 'SQL_MARKER_ABC' FROM missing_table", params: ['PARAM_MARKER_ABC'] },
  })) as unknown as { content: Array<{ text?: string }>; isError?: boolean };
  assert.equal(failed.isError, true);
  assert.doesNotMatch(failed.content.map((block) => block.text ?? '').join('\n'), /SQL_MARKER_ABC|PARAM_MARKER_ABC/);

  const rows = (await client.callTool({
    name: 'execute_sql',
    arguments: { sql: 'SELECT value FROM items' },
  })) as unknown as { structuredContent?: { rows?: unknown[][] }; isError?: boolean };
  assert.equal(rows.isError, undefined);
  assert.equal(rows.structuredContent?.rows?.[0]?.[0], 'ROW_MARKER_ABC');

  await client.close();
  await server.stop();
  const output = `${server.stdout()}\n${server.stderr()}`;
  assert.match(server.stdout(), /sqlite-mcp 已启动: http:\/\/127\.0\.0\.1:\d+\/mcp/);
  assert.doesNotMatch(output, /SQL_MARKER_ABC|PARAM_MARKER_ABC|ROW_MARKER_ABC/);
});
