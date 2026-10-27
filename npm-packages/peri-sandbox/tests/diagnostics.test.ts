import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, statSync, mkdirSync, symlinkSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { DiagnosticLog } from '../src/runtime/diagnostics.js';

test('diagnostic logs rotate within three bounded private files', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-diagnostics-'));
  const id = `sbx_${'a'.repeat(32)}`;
  try {
    const log = new DiagnosticLog(root, id, 5);
    log.write(Buffer.from('abcdefghijklm'));
    log.close();
    const base = join(root, 'diagnostics', id, 'diagnostic.log');
    assert.equal(readFileSync(base, 'utf8'), 'klm');
    assert.equal(readFileSync(`${base}.1`, 'utf8'), 'fghij');
    assert.equal(readFileSync(`${base}.2`, 'utf8'), 'abcde');
    for (const path of [base, `${base}.1`, `${base}.2`]) assert.equal(statSync(path).mode & 0o077, 0);
    assert.throws(() => new DiagnosticLog(root, 'invalid'));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('diagnostic log rejects symlinked or public parent paths', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-diagnostics-'));
  const id = `sbx_${'a'.repeat(32)}`;
  try {
    const outside = join(root, 'elsewhere');
    mkdirSync(outside);
    symlinkSync(outside, join(root, 'diagnostics'));
    assert.throws(() => new DiagnosticLog(root, id), /diagnostic root/);
    rmSync(join(root, 'diagnostics'));
    mkdirSync(join(root, 'diagnostics'), { mode: 0o755 });
    chmodSync(join(root, 'diagnostics'), 0o755);
    assert.throws(() => new DiagnosticLog(root, id), /private and real/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
