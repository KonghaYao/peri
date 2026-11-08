import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { replayProfile, validateReplayResult } from './replay-catalog.mjs';

const directory = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const record = args[0] === '--record';
if (record) args.shift();
const quickIndex = args.indexOf('--quick');
const quick = quickIndex !== -1;
if (quick) args.splice(quickIndex, 1);
if (args.includes('--record')) throw new Error('--record must precede Vitest arguments');
if (record && process.env.CI && process.env.CI !== 'false') throw new Error('CI is replay-only; recording fixtures is an explicit local action');
const profile = replayProfile(args, quick);

const invocation = process.env.PERI_E2E_REPLAY_INVOCATION ?? `${new Date().toISOString().replace(/[:.]/g, '-')}-${randomUUID().slice(0, 8)}`;
const results = path.resolve(process.env.PERI_E2E_REPLAY_RESULTS ?? path.join(directory, 'results/replay-v2', invocation));
await mkdir(results, { recursive: true });
const jsonReport = path.join(results, `vitest-${invocation}.json`);
const binaryPath = path.resolve(process.env.PERI_E2E_BINARY ?? path.join(directory, '../target/debug/peri'));
const binaryHash = createHash('sha256');
for await (const chunk of createReadStream(binaryPath)) binaryHash.update(chunk);
const binarySha256 = binaryHash.digest('hex');
const started = performance.now();
const child = spawn(process.execPath, [path.join(directory, 'node_modules/vitest/vitest.mjs'), 'run', '--config', 'vitest.replay-v3.config.ts', ...args,
  '--reporter=default', '--reporter=json', `--outputFile=${jsonReport}`], {
  cwd: directory,
  env: { ...process.env, PERI_E2E_BINARY: binaryPath, PERI_E2E_BINARY_SHA256: binarySha256,
    PERI_E2E_REPLAY_INVOCATION: invocation, PERI_E2E_REPLAY_RESULTS: results,
    PERI_E2E_REPLAY_MODE: record ? 'record' : 'replay', PERI_E2E_REPLAY_PROFILE: quick ? 'quick' : 'full' },
  stdio: 'inherit',
});
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
child.once('error', (error) => { console.error(error); process.exitCode = 1; });
child.once('close', async (code, signal) => {
  process.exitCode = code ?? (signal === 'SIGINT' ? 130 : 143);
  try {
    const result = JSON.parse(await readFile(jsonReport, 'utf8'));
    const executed = result.numPassedTests + result.numFailedTests;
    let gate;
    try { gate = validateReplayResult(result, profile); }
    catch (error) {
      gate = { status: 'failed', error: error.message };
      console.error(`Replay gate rejected: ${error.message}`);
      process.exitCode = 1;
    }
    await writeFile(path.join(results, `invocation-${invocation}.json`), JSON.stringify({
      invocation, mode: record ? 'record' : 'replay', profile, quickSelection: quick,
      binary: { path: binaryPath, sha256: binarySha256 },
      executed, passed: result.numPassedTests, failed: result.numFailedTests, skipped: result.numPendingTests,
      exitCode: process.exitCode, durationMs: performance.now() - started, jsonReport, gate,
    }, null, 2));
  } catch (error) {
    console.error(`Replay gate lacks a readable completion receipt: ${error.message}`);
    process.exitCode = 1;
  }
});
