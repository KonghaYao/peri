import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import type { Config } from '../src/config.js';
import { MacProvider } from '../src/runtime/mac-provider.js';

test('macOS stop refuses a process group with an unverified leader', {
  skip: process.platform !== 'darwin',
}, async () => {
  const controlRoot = mkdtempSync(join(tmpdir(), 'peri-mac-stop-'));
  try {
    mkdirSync(join(controlRoot, 'processes'));
    const config = { controlRoot, provider: { unitPrefix: 'peri-sbx-' }, limits: { stopMs: 100 } } as Config;
    const provider = new MacProvider(config);
    const unit = provider.unit(`sbx_${'a'.repeat(32)}`, `run_${'b'.repeat(24)}`);
    const group = Number(
      execFileSync('/bin/ps', ['-p', String(process.pid), '-o', 'pgid='], { encoding: 'utf8' }).trim(),
    );
    writeFileSync(join(controlRoot, 'processes', unit), JSON.stringify({ pid: group, started: 'wrong process' }));
    await assert.rejects(() => provider.stop(unit), /cannot confirm owned macOS process group/);
    process.kill(process.pid, 0);
  } finally {
    rmSync(controlRoot, { recursive: true, force: true });
  }
});
