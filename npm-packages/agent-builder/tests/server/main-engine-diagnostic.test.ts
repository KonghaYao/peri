import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { RivetError } from 'rivetkit';
import { engineStartupDiagnostic, rivetFailureGroup } from '../../server/chats/engine';

test('engine startup diagnostics carry whitelisted codes, health and ports only', () => {
  const diagnostic = engineStartupDiagnostic(
    { mode: 'embedded', status: 'unavailable' },
    ['store_unreachable'],
    9027,
  );

  expect(diagnostic).toEqual({
    code: 'engine_unavailable',
    status: 'unavailable',
    reasons: ['store_unreachable'],
    enginePort: 9027,
    hint: expect.stringContaining('PERI_ENGINE_PORT=9027'),
  });
  // The +1 port is the second listener the engine needs next to PERI_ENGINE_PORT.
  expect(diagnostic.hint).toContain('9028');
  expect(JSON.stringify(diagnostic)).not.toContain('.rivetkit');
  expect(diagnostic).not.toHaveProperty('message');
  expect(diagnostic).not.toHaveProperty('stack');
  expect(diagnostic).not.toHaveProperty('cause');
});

test('engine health reasons win over the configured status fallback', () => {
  const diagnostic = engineStartupDiagnostic(
    { mode: 'embedded', status: 'unavailable', reasons: ['store_unreachable'] },
    ['store_token_missing'],
    9030,
  );
  expect(diagnostic.reasons).toEqual(['store_unreachable']);
});

test('rivet failure groups are whitelisted and never echo an untrusted code or message', () => {
  const secret = `sk-${crypto.randomUUID()}`;
  const transport = new RivetError('client', 'connection_open_interrupted', secret);
  expect(rivetFailureGroup(transport)).toBe('client');

  // An unfamiliar group is dropped instead of being logged verbatim.
  const unknownGroup = Object.assign(new Error(secret), { group: secret, code: secret });
  expect(rivetFailureGroup(unknownGroup)).toBeNull();
  expect(rivetFailureGroup(new Error(secret))).toBeNull();
  expect(rivetFailureGroup({ code: secret, message: secret })).toBeNull();
});

test('main reports a failed engine start through the whitelisted diagnostic', () => {
  const source = readFileSync(new URL('../../server/main.ts', import.meta.url), 'utf8');
  const start = source.indexOf('void engine.start().catch(');
  const end = source.indexOf('const cleanup = createShutdown(');
  expect(start).toBeGreaterThan(-1);
  const block = source.slice(start, end);

  // The callback takes no error argument at all, so a message/stack cannot reach the log.
  expect(block).toContain('.catch(() => {');
  expect(block).toContain('engineStartupDiagnostic(engine.health()');
  expect(block).not.toContain('error.message');
  expect(block).not.toContain('error.stack');
});
