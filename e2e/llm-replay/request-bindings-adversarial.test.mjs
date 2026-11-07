import assert from 'node:assert/strict';
import test from 'node:test';
import { createBindingRegistry } from './bindings.mjs';
import { discoverBindingRules } from './request-bindings.mjs';

function registry(entries) {
  const value = createBindingRegistry({ runId: 'independent-audit' });
  for (const entry of entries) {
    value.declare({ owner: 'world', anchor: 'explicit test world', ...entry });
    value.bind({ owner: 'world', anchor: 'explicit test world', ...entry });
  }
  return value;
}

test('recorded text templates reject another session and preserve all literal words', () => {
  const bindings = registry([
    { kind: 'session', alias: 'A', recorded: 'recorded-A', live: 'new-A-longer' },
    { kind: 'session', alias: 'B', recorded: 'recorded-B', live: 'new-B' },
  ]);
  const body = { system: [{ type: 'text', text: 'Session ID: recorded-A\nOnly change A.' }] };
  const rules = discoverBindingRules(body, { registry: bindings, source: 'recorded' });
  assert.equal(rules.length, 1);
  assert.deepEqual(bindings.normalizeRequest({ system: [{ type: 'text', text: 'Session ID: new-A-longer\nOnly change A.' }] }, {
    owner: 'world', rules,
  }), body);
  for (const text of ['Session ID: new-B\nOnly change A.', 'Session ID: new-A-longer\nAlso change B.', 'Session ID: unknown\nOnly change A.']) {
    assert.throws(() => bindings.normalizeRequest({ system: [{ type: 'text', text }] }, { owner: 'world', rules }), { code: 'binding_value_mismatch' });
  }
});

test('discovery does not normalize token substrings or implicit derived task IDs', () => {
  const bindings = registry([
    { kind: 'session', alias: 'A', recorded: 'old-id', live: 'new-id' },
    { kind: 'path', alias: 'work', recorded: '/old/work', live: '/new/work' },
  ]);
  const body = {
    valid: 'session=old-id; file=/old/work/file.txt',
    notAnId: 'prefix-old-id and old-id-suffix and bg-old-id',
    notTheRoot: '/old/work-copy/file.txt and x/old/work/file.txt',
    traversal: '/old/work/../outside',
  };
  const rules = discoverBindingRules(body, { registry: bindings, source: 'recorded' });
  assert.deepEqual(rules.map((rule) => rule.path), ['/valid']);
  const rebound = bindings.rebindResponse(body, { owner: 'world', rules });
  assert.equal(rebound.valid, 'session=new-id; file=/new/work/file.txt');
  assert.equal(rebound.notAnId, body.notAnId);
  assert.equal(rebound.notTheRoot, body.notTheRoot);
  assert.equal(rebound.traversal, body.traversal);
});

test('only the labeled system environment date can be portable', () => {
  const bindings = registry([{ kind: 'date', alias: 'today', recorded: '2026-11-07', live: '2026-11-08' }]);
  const body = {
    system: '<env>\nToday\'s date: 2026-11-07\n</env>\nThe contract date is 2026-11-07.',
    messages: [{ role: 'user', content: '<env>\nToday\'s date: 2026-11-07\n</env>' }],
  };
  const rules = discoverBindingRules(body, { registry: bindings, source: 'recorded' });
  assert.deepEqual(rules.map((rule) => rule.path), ['/system']);
  const result = bindings.rebindResponse(body, { owner: 'world', rules });
  assert.equal(result.system, '<env>\nToday\'s date: 2026-11-08\n</env>\nThe contract date is 2026-11-07.');
  assert.deepEqual(result.messages, body.messages);
  assert.throws(() => registry([{ kind: 'date', alias: 'bad', recorded: '2026-02-30', live: '2026-11-08' }]), { code: 'invalid_binding_date' });
});

test('reasoning replay and provider signatures are excluded from discovery and explicit rewriting', () => {
  const bindings = registry([{ kind: 'path', alias: 'work', recorded: '/old/work', live: '/new/work' }]);
  const body = { messages: [{
    role: 'assistant',
    content: [
      { type: 'thinking', thinking: 'Inspect /old/work', signature: 'opaque /old/work' },
      { type: 'reasoning', summary: 'Inspect /old/work', encrypted_content: 'opaque /old/work' },
    ],
    reasoning_content: 'Inspect /old/work',
    thoughtSignature: '/old/work',
    thought_signature: '/old/work',
  }] };
  assert.deepEqual(discoverBindingRules(body, { registry: bindings, source: 'recorded' }), []);
  for (const property of ['reasoning_content', 'thoughtSignature', 'thought_signature']) {
    assert.throws(() => bindings.rebindResponse(body, { owner: 'world', rules: [{
      path: `/messages/0/${property}`, mode: 'typed-template',
      segments: [property === 'reasoning_content' ? 'Inspect ' : '', { kind: 'path', alias: 'work' }],
    }] }), { code: 'protected_binding_target' });
  }
});
