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
  const api = await import('../src/index.js');
  assert.equal(typeof api.connectSandboxControl, 'function');
  assert.equal(typeof api.newRequestId, 'function');
  assert.equal(typeof api.loadConfig, 'function');
  assert.equal(typeof api.startPeriSandbox, 'function');
});
