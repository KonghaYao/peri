#!/usr/bin/env node
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { connect } from '@tursodatabase/database';

const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const temporaryRoot = await mkdtemp(join(tmpdir(), 'sqlite-mcp-pack-'));
const packDirectory = join(temporaryRoot, 'pack');
const consumerDirectory = join(temporaryRoot, 'consumer');
let child;

function fail(message) {
  throw new Error(message);
}

async function runNodeBin(binPath, args, options = {}) {
  return execFileAsync(process.execPath, [binPath, ...args], {
    cwd: consumerDirectory,
    timeout: 30_000,
    maxBuffer: 8 * 1024 * 1024,
    ...options,
  });
}

async function waitForUrl(getStdout, childProcess) {
  return new Promise((resolveUrl, reject) => {
    const startedAt = Date.now();
    const timer = setInterval(() => {
      const match = getStdout().match(/http:\/\/127\.0\.0\.1:(\d+)\/mcp/);
      if (match?.[0] !== undefined) {
        clearInterval(timer);
        resolveUrl(match[0]);
      } else if (childProcess.exitCode !== null) {
        clearInterval(timer);
        reject(new Error(`packed CLI exited with ${childProcess.exitCode}`));
      } else if (Date.now() - startedAt > 20_000) {
        clearInterval(timer);
        reject(new Error('packed CLI did not start in time'));
      }
    }, 20);
  });
}

async function stopChild() {
  if (child === undefined || child.exitCode !== null) return;
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 3_000);
  await once(child, 'exit').catch(() => undefined);
  clearTimeout(timer);
}

try {
  await mkdir(packDirectory);
  await mkdir(consumerDirectory);
  const { stdout: packStdout } = await execFileAsync('npm', ['pack', '--json', '--pack-destination', packDirectory], {
    cwd: packageRoot,
    timeout: 180_000,
    maxBuffer: 8 * 1024 * 1024,
  });
  const packJson = JSON.parse(packStdout.slice(packStdout.indexOf('[')));
  const packed = packJson[0];
  if (packed === undefined) fail('npm pack returned no package');
  const tarball = resolve(packDirectory, packed.filename);
  const filePaths = (packed.files ?? []).map((file) => file.path);
  if (!filePaths.includes('dist/src/cli.js')) fail('tarball is missing dist/src/cli.js');
  if (filePaths.some((path) => path.startsWith('src/') || path.startsWith('tests/'))) {
    fail('tarball unexpectedly contains source or tests');
  }
  if (filePaths.some((path) => path.endsWith('.md'))) fail('tarball unexpectedly contains markdown');

  await execFileAsync('npm', ['install', '--no-audit', '--no-fund', tarball], {
    cwd: consumerDirectory,
    timeout: 240_000,
    maxBuffer: 8 * 1024 * 1024,
  });

  const installedRoot = join(consumerDirectory, 'node_modules', 'sqlite-mcp');
  const installed = JSON.parse(await readFile(join(installedRoot, 'package.json'), 'utf8'));
  const cliPath = join(installedRoot, installed.bin['sqlite-mcp']);
  const help = await runNodeBin(cliPath, ['--help']);
  if (!help.stdout.includes('sqlite-mcp <数据库路径>')) fail('installed CLI --help output is unexpected');

  if (process.platform !== 'win32') {
    const binShim = join(consumerDirectory, 'node_modules', '.bin', 'sqlite-mcp');
    const shimHelp = await execFileAsync(binShim, ['--help'], {
      cwd: consumerDirectory,
      timeout: 30_000,
      maxBuffer: 8 * 1024 * 1024,
    });
    if (!shimHelp.stdout.includes('sqlite-mcp <数据库路径>')) fail('installed bin shim --help output is unexpected');
  }

  const databasePath = join(temporaryRoot, 'installed.db');
  const database = await connect(databasePath);
  await database.exec(`
    CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
    CREATE VIEW user_names AS SELECT name FROM users;
    INSERT INTO users (name) VALUES ('packed');
  `);
  await database.close();

  let stdout = '';
  let stderr = '';
  child = spawn(process.execPath, [cliPath, databasePath, '--port', '0'], {
    cwd: consumerDirectory,
    env: { ...process.env, HOST: '0.0.0.0', PORT: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', (chunk) => {
    stdout += chunk;
  });
  child.stderr.on('data', (chunk) => {
    stderr += chunk;
  });
  const url = await waitForUrl(() => stdout, child);

  const client = new Client({ name: 'sqlite-mcp-pack-test', version: '1.0.0' });
  try {
    await client.connect(new StreamableHTTPClientTransport(new URL(url)));
    const tools = await client.listTools();
    const names = tools.tools.map((tool) => tool.name).sort();
    if (JSON.stringify(names) !== JSON.stringify(['describe_table', 'execute_sql', 'list_tables'])) {
      fail(`installed CLI exposed unexpected tools: ${names.join(',')}`);
    }
    const result = await client.callTool({
      name: 'execute_sql',
      arguments: { sql: 'SELECT name FROM users WHERE name = ?', params: ['packed'] },
    });
    if (result.structuredContent?.rows?.[0]?.[0] !== 'packed') fail('installed CLI returned unexpected rows');
    const tables = await client.callTool({ name: 'list_tables', arguments: {} });
    if (!tables.structuredContent?.tables?.some((table) => table.name === 'user_names' && table.type === 'view')) {
      fail('installed CLI did not list the view');
    }
  } finally {
    await client.close().catch(() => undefined);
  }

  await stopChild();
  if (stdout === '' || !stdout.includes('sqlite-mcp 已启动')) fail('packed CLI produced no startup line');
  if (stderr !== '') fail(`packed CLI wrote unexpected stderr: ${stderr}`);
  await stat(join(consumerDirectory, '.mcp-use')).then(
    () => fail('packed CLI wrote .mcp-use telemetry state'),
    () => undefined,
  );
  console.log(`PASS: npm pack + install + Node CLI call (${packed.filename}, tarball files: ${filePaths.length})`);
} finally {
  await stopChild();
  await rm(temporaryRoot, { recursive: true, force: true });
}
