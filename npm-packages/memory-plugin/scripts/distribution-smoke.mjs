import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

// Run after npm pack. BUN_BIN must identify the Bun binary to validate bunx.
const tarball = resolve(process.argv[2] || 'peri-memory-plugin-0.1.0.tgz');
const bun = process.env.BUN_BIN;
if (!bun) throw Error('Set BUN_BIN to a Bun executable');
const bytes = await readFile(tarball);
const cwd = await mkdtemp(join(tmpdir(), 'memory-distribution-'));
const env = { ...process.env, MCP_USE_ANONYMIZED_TELEMETRY: 'false',
  HINDSIGHT_BANK_ID: 'distribution-bank', MEMORY_AGENT_ID: 'distribution-agent',
  HINDSIGHT_API_KEY: 'distribution-key', MEMORY_SESSION_ID: 'distribution-session',
  MEMORY_STATE_DIR: join(cwd, 'state'), MEMORY_TIMEOUT_MS: '30000',
  MEMORY_MAX_TOKENS: '512', MEMORY_POLL_MS: '1000' };
// The fixture accepts only its own recall, so inherited plugin behavior must not add calls.
delete env.MEMORY_BOOTSTRAP_QUERY; delete env.MEMORY_ALLOW_HTTP; delete env.MEMORY_MCP_TOKEN;
let downloads = 0, recalls = 0;
const registry = createServer((req, res) => {
  downloads++; res.setHeader('content-type', 'application/octet-stream'); res.end(bytes);
});
const backend = createServer(async (req, res) => {
  const chunks = []; for await (const chunk of req) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks));
  assert.equal(req.url, '/v1/default/banks/distribution-bank/memories/recall');
  assert.equal(body.query, 'distribution smoke'); recalls++;
  res.setHeader('content-type', 'application/json');
  res.end(JSON.stringify({ results: [{ id: 'smoke', text: 'A packaged memory fact.' }] }));
});
const listen = server => new Promise(r => server.listen(0, '127.0.0.1', r));
const close = server => new Promise(r => { server.closeAllConnections(); server.close(r); });
async function run(command, args, input) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] });
    let out = '', err = '';
    child.stdout.on('data', data => { out += data; }); child.stderr.on('data', data => { err += data; });
    const timer = setTimeout(() => child.kill('SIGKILL'), 120000);
    child.on('error', error => { clearTimeout(timer); reject(error); });
    child.on('close', code => { clearTimeout(timer); code === 0 ? resolve(out) : reject(Error(`${command} exited ${code}: ${err}`)); });
    child.stdin.end(input);
  });
}
try {
  await listen(registry); await listen(backend);
  env.HINDSIGHT_BASE_URL = `http://127.0.0.1:${backend.address().port}`;
  const url = `http://127.0.0.1:${registry.address().port}/peri-memory-plugin-0.1.0.tgz`;
  const npmArgs = ['-y', `--package=${url}`];
  const bunArgs = ['x', '--bun', '--package', `@peri/memory-plugin@${url}`];
  assert.equal((await run('npx', [...npmArgs, 'memory-plugin', '--version'])).trim(), '0.1.0');
  console.log('PASS npx HTTP tarball installation and bin');
  assert.equal((await run(bun, [...bunArgs, 'memory-plugin', '--version'])).trim(), '0.1.0');
  console.log('PASS bunx --bun HTTP tarball installation and bin');
  assert.ok(downloads >= 2);
  await close(registry);
  const runners = [
    { name: 'npx --offline', command: 'npx', args: ['--offline', ...npmArgs] },
    { name: 'bunx --no-install --bun', command: bun, args: ['x', '--no-install', ...bunArgs.slice(1)] },
  ];
  for (const runner of runners) {
    assert.equal((await run(runner.command, [...runner.args, 'memory-plugin', '--version'])).trim(), '0.1.0');
    console.log(`PASS ${runner.name}: cached bin with tarball origin stopped`);
    const client = new Client({ name: 'distribution-check', version: '1' }, { versionNegotiation: { mode: { pin: '2026-07-28' } } });
    try {
      await client.connect(new StdioClientTransport({ command: runner.command,
        args: [...runner.args, 'memory-plugin', 'mcp'], cwd, env, stderr: 'pipe' }));
      assert.equal((await client.listTools()).tools.length, 4);
      assert.equal((await client.callTool({ name: 'memory_recall', arguments: { query: 'distribution smoke' } })).isError, false);
      console.log(`PASS ${runner.name}: packaged stdio 0728 discovery and real HTTP recall adapter`);
    } finally { await client.close(); }
    const data = { schemaVersion: 1, agentId: env.MEMORY_AGENT_ID, sessionId: env.MEMORY_SESSION_ID,
      records: [{ id: 'sample', kind: 'compact', timestamp: '2026-11-16T00:00:00Z', content: 'Packaging test only.' }] };
    const receipt = JSON.parse((await run(runner.command, [...runner.args, 'memory-plugin', 'import', '--file', '-', '--dry-run'], JSON.stringify(data))).trim().split('\n').at(-1));
    assert.equal(receipt.dryRun, true); assert.equal(receipt.completed, 0);
    console.log(`PASS ${runner.name}: independent import CLI stdin dry-run`);
  }
  assert.equal(recalls, 2);
  console.log(`PASS distribution summary: ${downloads} tarball downloads, ${recalls} fixture recalls; no deployed Hindsight used`);
} finally {
  if (registry.listening) await close(registry);
  if (backend.listening) await close(backend);
  await rm(cwd, { recursive: true, force: true });
}
