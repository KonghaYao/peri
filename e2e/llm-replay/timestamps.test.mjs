import assert from 'node:assert/strict';
import test from 'node:test';
import { createBindingRegistry } from './bindings.mjs';
import { discoverBindingRules, mergeBindingRules } from './request-bindings.mjs';

const a = '2026-11-08T00:00:00.123Z';
const b = '2026-11-09T02:00:00.999999999+02:00';
const declaration = (alias, recorded = a) => ({ owner: 'world', kind: 'timestamp', alias, recorded, anchor: `monitor/task/event/${alias}` });
const ref = (alias) => ({ kind: 'timestamp', alias });

test('业务时间仅能通过已验证事件的显式字段模板转换，其余业务正文保持严格', () => {
  const registry = createBindingRegistry({ runId: 'new', declarations: [declaration('start')] });
  registry.bind({ ...declaration('start'), live: b });
  const body = { result: `{"ts":"${b}","text":"strict event"}`, user: b };
  assert.deepEqual(discoverBindingRules(body, { registry }), [], '工具时间和同形用户文本都不能自动忽略');
  const rules = [{ path: '/result', mode: 'typed-template', segments: ['{"ts":"', ref('start'), '","text":"strict event"}'] }];
  assert.deepEqual(registry.normalizeRequest(body, { owner: 'world', rules }), { result: `{"ts":"${a}","text":"strict event"}`, user: b });
  assert.throws(() => registry.normalizeRequest({ ...body, result: body.result.replace('strict event', 'wrong event') }, { owner: 'world', rules }), { code: 'binding_value_mismatch' });
  assert.throws(() => registry.bind({ ...declaration('start'), live: a, anchor: 'another-task' }), { code: 'binding_anchor_mismatch' });
});

test('同毫秒不同事件可按独立锚点绑定，重放时相等关系无需成为隐含身份', () => {
  const registry = createBindingRegistry({ runId: 'same-time', declarations: [declaration('start'), declaration('finish')] });
  registry.bind({ ...declaration('start'), live: b });
  registry.bind({ ...declaration('finish'), live: '2026-11-09T02:00:01+02:00' });
  const rules = [
    { path: '/start', ...ref('start') },
    { path: '/finish', ...ref('finish') },
  ];
  assert.deepEqual(registry.normalizeRequest({ start: b, finish: '2026-11-09T02:00:01+02:00' }, { owner: 'world', rules }), { start: a, finish: a });
  assert.throws(() => registry.bind({ ...declaration('start'), live: '2026-11-09T02:00:02+02:00' }), { code: 'binding_alias_rebound' });
});

test('拒绝无时区、越界日期时分秒、非法偏移与非时间内容', () => {
  for (const invalid of ['hello', '2026-11-08', '2026-11-08T01:02:03', '2026-02-30T00:00:00Z',
    '2026-11-08T24:00:00Z', '2026-11-08T01:60:00Z', '2026-11-08T01:00:60Z', '2026-11-08T01:00:00+24:00', '2026-11-08T01:00:00+02:60']) {
    assert.throws(() => createBindingRegistry({ runId: 'invalid', declarations: [declaration('bad', invalid)] }), { code: 'invalid_binding_timestamp' });
  }
});

test('显式复合模板仅覆盖同pointer自动规则，不能删除其它字段约束或重复声明', () => {
  const automatic = [{ path: '/result', mode: 'typed-template', segments: ['task=', { kind: 'task', alias: 'job' }] },
    { path: '/system', kind: 'session', alias: 'session' }];
  const explicit = { path: '/result', mode: 'typed-template', segments: ['task=', { kind: 'task', alias: 'job' }, ';ts=', ref('start')] };
  assert.deepEqual(mergeBindingRules(automatic, [explicit]), [automatic[1], explicit]);
  assert.throws(() => mergeBindingRules(automatic, [explicit, explicit]), { code: 'duplicate_binding_rule' });
  assert.throws(() => mergeBindingRules(automatic, [{ path: '' }]), { code: 'invalid_binding_pointer' });
});
