import assert from 'node:assert/strict';
import test from 'node:test';
import { beginOpen, matchesOpening, terminalCanCommit, shouldClearOpening, shouldIgnoreLateAck } from '../src/panel/lib/open-state.mjs';
const parsePrincipal = (v) => v && ['full','read-only'].includes(v.role) ? v.role : null;
const canMutate = (role) => role === 'full';

test('opening a session preserves selection until its terminal ack', async () => {
  const opening = beginOpen('cmd', 'new', 'old', 'chat-old');
  assert.deepEqual(opening, { commandId:'cmd',sessionId:'new',previousSessionId:'old',previousChatId:'chat-old' });
  assert.equal(terminalCanCommit(opening,{commandId:'cmd',status:'committed',chatId:'chat-new'}),true);
  assert.equal(terminalCanCommit(opening,{commandId:'cmd',status:'duplicate',chatId:'chat-new'}),true);
});

test('stale terminal ack cannot switch a newer opening request', async () => {
  const newer=beginOpen('b','session-b','old','chat-old');
  assert.equal(terminalCanCommit(newer,{commandId:'a',status:'committed',chatId:'chat-a'}),false);
  assert.equal(shouldClearOpening(newer,'a'),false);
  assert.equal(matchesOpening(newer,'b'),true);
});

test('principal parsing and mutation policy are closed by default', async () => {
  assert.equal(parsePrincipal({ role: 'full' }), 'full');
  assert.equal(parsePrincipal({ role: 'read-only' }), 'read-only');
  assert.equal(parsePrincipal({ role: 'instance' }), null);
  assert.equal(canMutate('full'), true);
  assert.equal(canMutate('read-only'), false);
  assert.equal(canMutate(null), false);
});
test('timed out open rejects a late terminal ack', () => {
  const ignored = new Set(['late']);
  assert.equal(shouldIgnoreLateAck(ignored,{commandId:'late',status:'committed',chatId:'wrong'}),true);
  assert.equal(shouldIgnoreLateAck(ignored,{commandId:'other',status:'committed',chatId:'ok'}),false);
});
