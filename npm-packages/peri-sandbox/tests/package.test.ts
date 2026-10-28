import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import test from 'node:test';

test('declared CLI and public TypeScript entry resolve to built files', async () => {
  const config = JSON.parse(readFileSync('package.json', 'utf8')) as {
    bin: Record<string, string>;
    main: string;
    types: string;
    exports: { '.': { types: string; default: string } };
  };
  for (const path of [
    config.bin['peri-sandbox'],
    config.main,
    config.types,
    config.exports['.'].types,
    config.exports['.'].default,
  ]) {
    assert(existsSync(path), `missing package entrypoint: ${path}`);
  }
  const api = (await import('../src/index.js')) as Record<string, unknown>;
  for (const symbol of [
    'startSandbox',
    'deleteSandboxData',
    'initSettings',
    'loadSettings',
    'defaultSettingsPath',
    'createPeriSandboxAdapter',
  ])
    assert.equal(typeof api[symbol], 'function', `missing export: ${symbol}`);
  assert.equal(typeof api.SandboxError, 'function');
  for (const removed of ['connectSandboxControl', 'newRequestId', 'startPeriSandbox', 'loadConfig', 'ControlError'])
    assert.equal(api[removed], undefined, `removed export still present: ${removed}`);
});
