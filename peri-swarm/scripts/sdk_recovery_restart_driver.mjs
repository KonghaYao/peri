#!/usr/bin/env node
// Real active-turn restart regression. No fake HTTP gateway or state fixture.
//
// Start the existing Loom/Rivet/Bun/model stub first in this network namespace.
// The stub must have PERI_STUB_CHUNK_DELAY_MS=1500 and PERI_STUB_COMMANDS with
// {"once":"printf 'once\\n' >> ran.log; printf 'once\\n' > once.txt"}.
// Supply normal gateway environment (including PERI_SWARM_BIND and stores),
// PERI_SWARM_BASE_URL, PERI_SWARM_API_KEY, PERI_SWARM_BUN_TOKEN, NODE_PATH for
// official openai@7.60.0, and PERI_E2E_GATEWAY_BIN (absolute executable path).
// Rivet must be external: PERI_SWARM_RIVET_ENGINE_SPAWN=never.
//
// Optional: PERI_E2E_GATEWAY_ARGS='[]', PERI_E2E_GATEWAY_CWD,
// PERI_E2E_RUN_DIR (must not exist), PERI_E2E_TIMEOUT_MS (default 120000).
// The target gateway port must be unused. Only exact child PIDs created by
// this driver are killed; all prerequisite services remain running.
// Evidence persists in RUN_DIR. Exit 0 requires BOTH verifier modes and the
// real SIGKILL/restart lifecycle to pass. Syntax/prerequisite checks alone are
// never evidence of restart E2E success.

import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { execFileSync, spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

if (process.argv.includes('--help')) {
  console.log('See the header in sdk_recovery_restart_driver.mjs for prerequisites and environment. This runs a real hosted active-turn crash/restart; it owns the gateway process.');
  process.exit(0);
}

const root = fileURLToPath(new URL('../..', import.meta.url));
const verifier = fileURLToPath(new URL('./sdk_restart_boundary_e2e.mjs', import.meta.url));
const binary = process.env.PERI_E2E_GATEWAY_BIN;
const baseURL = new URL(process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:18080/v1');
const timeout = Number(process.env.PERI_E2E_TIMEOUT_MS ?? '120000');
const runDir = process.env.PERI_E2E_RUN_DIR ?? path.join('/tmp', `peri-active-restart-${crypto.randomUUID()}`);
const timeline = [];
const gateways = [];
let gateway;
let failure = null;
let source = null;

function mark(event, detail = {}) {
  timeline.push({ at: new Date().toISOString(), event, ...detail });
}
function sleep(ms) { return new Promise((resolve) => setTimeout(resolve, ms)); }
function assert(condition, message) { if (!condition) throw new Error(message); }
function write(name, value) {
  fs.writeFileSync(path.join(runDir, name), `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
}
function isPortOpen() {
  return new Promise((resolve) => {
    const socket = net.connect({ host: baseURL.hostname, port: Number(baseURL.port) || 80 });
    socket.once('connect', () => { socket.destroy(); resolve(true); });
    socket.once('error', () => { socket.destroy(); resolve(false); });
    socket.setTimeout(1000, () => { socket.destroy(); resolve(false); });
  });
}
function exited(child) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => child.once('exit', resolve));
}
async function stop(child, signal) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const done = exited(child);
  assert(child.kill(signal), `could not signal owned gateway PID ${child.pid}`);
  await Promise.race([done, sleep(5000)]);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await done;
  }
  mark('gateway_exited', { pid: child.pid, exitCode: child.exitCode, signal: child.signalCode });
}
async function launch(label) {
  const logPath = path.join(runDir, `${label}.log`);
  const log = fs.openSync(logPath, 'wx', 0o600);
  const child = spawn(binary, JSON.parse(process.env.PERI_E2E_GATEWAY_ARGS ?? '[]'), {
    cwd: process.env.PERI_E2E_GATEWAY_CWD ?? root,
    env: { ...process.env, PERI_SWARM_RIVET_ENGINE_SPAWN: 'never' },
    stdio: ['ignore', log, log],
  });
  fs.closeSync(log);
  let launchError;
  child.on('error', (error) => { launchError = error; });
  gateways.push(child);
  gateway = child;
  mark('gateway_spawned', { label, pid: child.pid, logPath });
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    assert(!launchError, `${label} could not execute the gateway: ${launchError?.message}`);
    assert(child.exitCode === null && child.signalCode === null, `${label} exited before readiness; see ${logPath}`);
    try {
      const response = await fetch(new URL('/healthz', baseURL), { signal: AbortSignal.timeout(1500) });
      if (response.ok) { mark('gateway_ready', { pid: child.pid }); return child; }
    } catch { /* readiness polling */ }
    await sleep(100);
  }
  throw new Error(`${label} readiness timed out; see ${logPath}`);
}
async function verify(mode) {
  const recordPath = path.join(runDir, `${mode}.json`);
  const stdout = fs.openSync(path.join(runDir, `${mode}.stdout.json`), 'wx', 0o600);
  const stderr = fs.openSync(path.join(runDir, `${mode}.stderr.log`), 'wx', 0o600);
  const child = spawn(process.execPath, [verifier, mode], {
    cwd: root,
    env: { ...process.env, PERI_E2E_STATE: path.join(runDir, 'state.json'), PERI_E2E_OUT: recordPath },
    stdio: ['ignore', stdout, stderr],
  });
  fs.closeSync(stdout);
  fs.closeSync(stderr);
  let launchError;
  child.on('error', (error) => { launchError = error; });
  const timer = setTimeout(() => child.kill('SIGKILL'), timeout + 10000);
  try { await exited(child); } finally { clearTimeout(timer); }
  assert(!launchError && child.exitCode === 0, `${mode} failed (${launchError?.message ?? child.exitCode ?? child.signalCode}); see ${recordPath}`);
  const record = JSON.parse(fs.readFileSync(recordPath, 'utf8'));
  assert(record.checks?.length > 0 && record.failures?.length === 0, `${mode} produced incomplete evidence`);
  mark('verification_passed', { mode, checkCount: record.checks.length });
  return record;
}

try {
  assert(binary && path.isAbsolute(binary) && fs.statSync(binary).isFile(), 'PERI_E2E_GATEWAY_BIN must name the absolute real gateway executable');
  fs.accessSync(binary, fs.constants.X_OK);
  assert(['127.0.0.1', 'localhost', '[::1]'].includes(baseURL.hostname), 'this destructive local bench only accepts a loopback gateway');
  assert(!fs.existsSync(runDir), 'PERI_E2E_RUN_DIR must be fresh');
  fs.mkdirSync(runDir, { recursive: true, mode: 0o700 });
  assert(!(await isPortOpen()), 'gateway target port is already occupied; refusing to kill or replace an unowned service');
  const binarySha256 = crypto.createHash('sha256').update(fs.readFileSync(binary)).digest('hex');
  const git = (args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
  source = {
    commit: git(['rev-parse', 'HEAD']),
    branch: git(['rev-parse', '--abbrev-ref', 'HEAD']),
    trackedDiffSha256: crypto.createHash('sha256').update(git(['diff', 'HEAD', '--'])).digest('hex'),
    verifierSha256: crypto.createHash('sha256').update(fs.readFileSync(verifier)).digest('hex'),
    driverSha256: crypto.createHash('sha256').update(fs.readFileSync(fileURLToPath(import.meta.url))).digest('hex'),
  };
  mark('prerequisites_checked', { binary, binarySha256 });
  const first = await launch('gateway-before');
  const prepared = await verify('active-prepare');
  const killBoundaryResponse = await fetch(new URL(`agents/sessions/${prepared.sessionId}`, `${baseURL.href.replace(/\/$/, '')}/`), {
    headers: { authorization: `Bearer ${process.env.PERI_SWARM_API_KEY}`, 'x-peri-swarm-expect': '1', 'OpenAI-Beta': 'agents=v1' },
    signal: AbortSignal.timeout(5000),
  });
  const killBoundary = await killBoundaryResponse.json();
  assert(killBoundaryResponse.ok && killBoundary.state === 'running', 'the real turn finished before SIGKILL; increase stub chunk delay');
  mark('running_observed_immediately_before_kill', { pid: first.pid, sessionId: prepared.sessionId, lastItemSeq: killBoundary.lastItemSeq });
  await stop(first, 'SIGKILL');
  assert(first.signalCode === 'SIGKILL', 'the active boundary did not terminate by SIGKILL');
  mark('active_gateway_killed', { pid: first.pid, sessionId: prepared.sessionId, preparedAt: prepared.preparedAt });
  const second = await launch('gateway-after');
  assert(first.pid !== second.pid, 'gateway restart did not change PID');
  await verify('active-after');
} catch (error) {
  failure = error?.message ?? String(error);
} finally {
  for (const child of gateways) await stop(child, 'SIGINT').catch((error) => { failure ??= error.message; });
}
const record = {
  scenario: 'real-hosted-active-turn-gateway-restart', ok: failure === null,
  failure, runDir, timeline,
  source,
  runtime: {
    sandboxBackend: process.env.PERI_E2E_SANDBOX_BACKEND ?? process.env.PERI_STACK_BUN_ENTRY ?? 'default production entry',
    periProviderBinary: process.env.PERI_PATH ?? null,
  },
  coverage: { activeTurnRestart: failure === null, childMqRecovery: 'not exercised by this hosted root-turn bench' },
};
if (fs.existsSync(runDir)) write('driver.json', record);
console.log(JSON.stringify(record, null, 2));
process.exit(failure === null ? 0 : 1);
