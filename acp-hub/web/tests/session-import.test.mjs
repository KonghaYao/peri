import test from 'node:test';
import assert from 'node:assert/strict';
import { importCandidates, unimportedSessions } from '../src/panel/lib/session-import.mjs';

test('sidebar import candidates are cwd-scoped and exclude catalog sessions', () => {
  const sessions = [
    { sessionId: 'a', cwd: '/repo' },
    { sessionId: 'b', cwd: '/repo' },
    { sessionId: 'c', cwd: '/other' },
  ];
  const catalog = [{ acpSessionId: 'a' }];
  const unimported = unimportedSessions(sessions, catalog);
  assert.deepEqual(importCandidates(unimported, '/repo').map((item) => item.sessionId), ['b']);
});
