import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';
import { BindingError, createBindingRegistry, derivePortableSse, normalizeBody } from './bindings.mjs';

const childRule = { path: '/thread_id', kind: 'thread', alias: 'child-A' };

function makeRegistry(runId = 'run-1', live = 'live-child-A-with-a-longer-id') {
  const registry = createBindingRegistry({ runId, declarations: [
    { owner: 'root-A', kind: 'thread', alias: 'child-A', recorded: 'old-child-A', anchor: 'root-A/call-1/tool-A' },
    { owner: 'root-B', kind: 'thread', alias: 'child-B', recorded: 'old-child-B', anchor: 'root-B/call-1/tool-B' },
    { owner: 'root-A', kind: 'path', alias: 'workspace', recorded: '/recorded/work', anchor: 'scenario/workspace-A' },
  ] });
  registry.bind({ owner: 'root-A', kind: 'thread', alias: 'child-A', live, anchor: 'root-A/call-1/tool-A' });
  registry.bind({ owner: 'root-A', kind: 'path', alias: 'workspace', live: '/new/run/workspace', anchor: 'scenario/workspace-A' });
  return registry;
}

function errorCode(code) {
  return (error) => error instanceof BindingError && error.code === code;
}

function frame(data) {
  return `event: ${data.type}\ndata: ${JSON.stringify(data)}\n\n`;
}

function events(body) {
  return body.toString().split('\n\n').filter(Boolean).map((block) => JSON.parse(block.split('\n').find((line) => line.startsWith('data:')).slice(5).trim()));
}

function makeAnthropic({ args = { thread_id: 'old-child-A', note: '中文' }, terminal = true, incomplete = false } = {}) {
  const text = JSON.stringify(args);
  const parts = incomplete ? ['{"thread_id":"old-'] : [text.slice(0, 21), text.slice(21, 26), text.slice(26)];
  return Buffer.from([
    frame({ type: 'message_start', message: { content: [] } }),
    frame({ type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '', signature: 'opaque-signature' } }),
    frame({ type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: 'old-child-A' } }),
    frame({ type: 'content_block_stop', index: 0 }),
    frame({ type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id: 'tool-resume', name: 'resume', input: {} } }),
    ...parts.map((partial_json) => frame({ type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json } })),
    frame({ type: 'content_block_stop', index: 1 }),
    ...(terminal ? [frame({ type: 'message_stop' })] : []),
  ].join(''));
}

function makeResponses({ terminalArgs, unknownEvent = false } = {}) {
  const args = JSON.stringify({ thread_id: 'old-child-A', note: '中文' });
  const item = { type: 'function_call', id: 'item-1', call_id: 'tool-resume', name: 'resume', arguments: args };
  return Buffer.from([
    frame({ type: 'response.created', response: { id: 'response-1' } }),
    frame({ type: 'response.output_item.added', output_index: 0, item: { ...item, arguments: '' } }),
    frame({ type: 'response.function_call_arguments.delta', output_index: 0, item_id: 'item-1', delta: args.slice(0, 19) }),
    frame({ type: 'response.function_call_arguments.delta', output_index: 0, item_id: 'item-1', delta: args.slice(19) }),
    ...(unknownEvent ? [frame({ type: 'response.future_arguments.snapshot', item_id: 'item-1', arguments: args })] : []),
    frame({ type: 'response.function_call_arguments.done', output_index: 0, item_id: 'item-1', arguments: args }),
    frame({ type: 'response.output_item.done', output_index: 0, item }),
    frame({ type: 'response.completed', response: { id: 'response-1', output: [
      { ...item, arguments: terminalArgs ?? args },
      { type: 'reasoning', id: 'reason-1', summary: [], encrypted_content: 'old-child-A-opaque-encrypted' },
    ] } }),
  ].join(''));
}

function portable(raw, protocol = 'anthropic', extras = {}) {
  return derivePortableSse({ raw, protocol, registry: makeRegistry(), owner: 'root-A',
    tools: [{ toolCallId: 'tool-resume', rules: [childRule] }],
    rulesVersion: 'test-v1', fragmentPlan: { kind: 'utf8', maxBytes: 7 }, ...extras });
}

test('按声明锚点反序绑定两个子会话，不能按 HTTP 到达顺序互换', () => {
  const registry = createBindingRegistry({ runId: 'reverse', declarations: [
    { owner: 'root', kind: 'thread', alias: 'A', recorded: 'recorded-A', anchor: 'call/tool-A' },
    { owner: 'root', kind: 'thread', alias: 'B', recorded: 'recorded-B', anchor: 'call/tool-B' },
  ] });
  registry.bind({ owner: 'root', kind: 'thread', alias: 'B', live: 'live-B', anchor: 'call/tool-B' });
  registry.bind({ owner: 'root', kind: 'thread', alias: 'A', live: 'live-A-long', anchor: 'call/tool-A' });
  const body = { a: 'live-A-long', b: 'live-B', content: 'unchanged', is_error: true };
  const rules = [
    { path: '/a', kind: 'thread', alias: 'A' },
    { path: '/b', kind: 'thread', alias: 'B' },
  ];
  const normalized = registry.normalizeRequest(body, { owner: 'root', rules });
  assert.deepEqual(normalized, { a: 'recorded-A', b: 'recorded-B', content: 'unchanged', is_error: true });
  assert.deepEqual(registry.rebindResponse(normalized, { owner: 'root', rules }), body);
  assert.equal(body.a, 'live-A-long', '输入对象不能被原地改写');
});

test('不同 run 的相同 cassette 别名分别绑定，不能共享游标式全局状态', () => {
  const a = makeRegistry('run-A', 'uuid-A');
  const b = makeRegistry('run-B', 'uuid-B');
  assert.equal(normalizeBody({ thread_id: 'uuid-A' }, a, 'root-A', [childRule]).thread_id, 'old-child-A');
  assert.equal(normalizeBody({ thread_id: 'uuid-B' }, b, 'root-A', [childRule]).thread_id, 'old-child-A');
  assert.throws(() => normalizeBody({ thread_id: 'uuid-A' }, b, 'root-A', [childRule]), errorCode('binding_value_mismatch'));
});

test('错误锚点、跨 owner 引用、多个别名合并为一个实际身份均拒绝', () => {
  const registry = makeRegistry();
  assert.throws(() => registry.bind({ owner: 'root-B', kind: 'thread', alias: 'child-B', live: 'live-B', anchor: 'wrong/tool' }), errorCode('binding_anchor_mismatch'));
  assert.throws(() => registry.bind({ owner: 'root-B', kind: 'thread', alias: 'child-B', live: 'live-child-A-with-a-longer-id', anchor: 'root-B/call-1/tool-B' }), errorCode('binding_alias_collision'));
  assert.throws(() => registry.rebindResponse({ thread_id: 'old-child-A' }, { owner: 'root-B', rules: [childRule] }), errorCode('binding_not_declared'));
  assert.throws(() => registry.rebindResponse({ thread_id: 'old-child-A' }, { owner: 'root-A', rules: [{ ...childRule, owner: 'root-B' }] }), errorCode('binding_owner_mismatch'));
});

test('相同别名只能幂等登记相同 live 值，未声明或未绑定引用失败', () => {
  const registry = makeRegistry();
  assert.equal(registry.bind({ owner: 'root-A', kind: 'thread', alias: 'child-A', live: 'live-child-A-with-a-longer-id', anchor: 'root-A/call-1/tool-A' }).live, 'live-child-A-with-a-longer-id');
  assert.throws(() => registry.bind({ owner: 'root-A', kind: 'thread', alias: 'child-A', live: 'different', anchor: 'root-A/call-1/tool-A' }), errorCode('binding_alias_rebound'));
  assert.throws(() => registry.bind({ owner: 'root-A', kind: 'thread', alias: 'unknown', live: 'x', anchor: 'x' }), errorCode('binding_not_declared'));
  assert.throws(() => registry.rebindResponse({ id: 'old-child-B' }, { owner: 'root-B', rules: [{ path: '/id', kind: 'thread', alias: 'child-B' }] }), errorCode('binding_unbound'));
});

test('录制身份不能以多个 owner 或别名声明，规则不做模糊匹配', () => {
  const registry = makeRegistry();
  assert.throws(() => registry.declare({ owner: 'root-B', kind: 'thread', alias: 'other', recorded: 'old-child-A', anchor: 'other' }), errorCode('binding_alias_collision'));
  assert.throws(() => registry.rebindResponse({ thread_id: 'other-recorded' }, { owner: 'root-A', rules: [childRule] }), errorCode('binding_value_mismatch'));
  assert.throws(() => registry.rebindResponse({}, { owner: 'root-A', rules: [childRule] }), errorCode('binding_path_missing'));
});

test('工作区前缀仅在显式路径字段转换，邻接路径、穿越和 Bash 字符串失败', () => {
  const registry = makeRegistry();
  const rules = [{ path: '/file', kind: 'path', alias: 'workspace', mode: 'path-prefix' }];
  const body = { file: '/new/run/workspace/src/main.rs', command: 'cat /new/run/workspace/src/main.rs' };
  const normalized = registry.normalizeRequest(body, { owner: 'root-A', rules });
  assert.equal(normalized.file, '/recorded/work/src/main.rs');
  assert.equal(normalized.command, body.command);
  assert.equal(registry.rebindResponse(normalized, { owner: 'root-A', rules }).file, body.file);
  for (const value of ['/new/run/workspace-other/f', '/new/run/workspace/../outside', 'cat /new/run/workspace/f']) {
    assert.throws(() => registry.normalizeRequest({ file: value }, { owner: 'root-A', rules }), errorCode('binding_value_mismatch'));
  }
});

test('归一化不会改写 signed/opaque 块或未声明的普通文本', () => {
  const registry = makeRegistry();
  for (const body of [{ type: 'thinking', text: 'old-child-A' }, { type: 'reasoning', text: 'old-child-A' }]) {
    assert.throws(() => registry.rebindResponse(body, { owner: 'root-A', rules: [{ ...childRule, path: '/text' }] }), errorCode('protected_binding_target'));
  }
  assert.throws(() => registry.rebindResponse({ signature: 'old-child-A' }, { owner: 'root-A', rules: [{ ...childRule, path: '/signature' }] }), errorCode('protected_binding_target'));
  assert.throws(() => registry.rebindResponse({ encrypted_content: 'old-child-A' }, { owner: 'root-A', rules: [{ ...childRule, path: '/encrypted_content' }] }), errorCode('protected_binding_target'));
  assert.deepEqual(registry.rebindResponse({ thread_id: 'old-child-A', text: 'old-child-A' }, { owner: 'root-A', rules: [childRule] }),
    { thread_id: 'live-child-A-with-a-longer-id', text: 'old-child-A' });
});

test('精确 JSON Pointer 支持转义，拒绝通配符、原型路径及重复规则', () => {
  const registry = makeRegistry();
  assert.equal(registry.rebindResponse({ 'a/b': { '~id': 'old-child-A' } }, { owner: 'root-A', rules: [{ ...childRule, path: '/a~1b/~0id' }] })['a/b']['~id'], 'live-child-A-with-a-longer-id');
  for (const pointer of ['/a/*', '/__proto__/x', '/a~2b']) {
    assert.throws(() => registry.rebindResponse({}, { owner: 'root-A', rules: [{ ...childRule, path: pointer }] }), errorCode('invalid_binding_pointer'));
  }
  assert.throws(() => registry.rebindResponse({ thread_id: 'old-child-A' }, { owner: 'root-A', rules: [childRule, childRule] }), errorCode('duplicate_binding_rule'));
});

test('Anthropic 跨 delta 旧 UUID 重绑定为不同长度新 UUID，原录制及 thinking 字节不变', () => {
  const raw = makeAnthropic();
  const original = Buffer.from(raw);
  const { body, provenance } = portable(raw);
  const parsed = events(body);
  const args = parsed.filter((event) => event.type === 'content_block_delta' && event.index === 1).map((event) => event.delta.partial_json).join('');
  assert.deepEqual(JSON.parse(args), { thread_id: 'live-child-A-with-a-longer-id', note: '中文' });
  assert.ok(parsed.filter((event) => event.type === 'content_block_delta' && event.index === 1).length > 3, '必须使用显式新分片计划');
  const signed = frame({ type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '', signature: 'opaque-signature' } });
  assert.ok(body.includes(signed));
  assert.deepEqual(raw, original);
  assert.equal(provenance.sourceSha256, createHash('sha256').update(raw).digest('hex'));
  assert.equal(provenance.rawChunkBoundaries, 'not-preserved');
  assert.equal(provenance.timing, 'not-preserved');
});

test('完整 Anthropic input 无 delta 时仍按声明转换，不生成额外流式事件', () => {
  const raw = Buffer.from([
    frame({ type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id: 'tool-resume', name: 'resume', input: { thread_id: 'old-child-A' } } }),
    frame({ type: 'content_block_stop', index: 0 }), frame({ type: 'message_stop' }),
  ].join(''));
  const parsed = events(portable(raw).body);
  assert.equal(parsed[0].content_block.input.thread_id, 'live-child-A-with-a-longer-id');
  assert.equal(parsed.length, 3);
});

test('半截工具 JSON、缺终态和半个 UTF-8 字符不能被 portable 派生补全', () => {
  assert.throws(() => portable(makeAnthropic({ incomplete: true })), errorCode('incomplete_tool_arguments'));
  assert.throws(() => portable(makeAnthropic({ terminal: false })), errorCode('incomplete_sse'));
  assert.throws(() => portable(Buffer.concat([makeAnthropic(), Buffer.from([0xe4, 0xb8])])), errorCode('incomplete_sse'));
});

test('未知工具引用、未支持协议和未声明分片计划必须失败', () => {
  assert.throws(() => portable(makeAnthropic(), 'anthropic', { tools: [{ toolCallId: 'unknown', rules: [childRule] }] }), errorCode('unknown_tool_reference'));
  assert.throws(() => portable(makeAnthropic(), 'openai-chat'), errorCode('unsupported_portable_protocol'));
  assert.throws(() => portable(makeAnthropic(), 'anthropic', { fragmentPlan: undefined }), errorCode('invalid_fragment_plan'));
});

test('Responses delta、arguments.done、item.done、completed 四份参数同步重绑定', () => {
  const raw = makeResponses();
  const original = Buffer.from(raw);
  const parsed = events(portable(raw, 'openai-responses').body);
  const expected = { thread_id: 'live-child-A-with-a-longer-id', note: '中文' };
  const delta = parsed.filter((event) => event.type === 'response.function_call_arguments.delta').map((event) => event.delta).join('');
  assert.deepEqual(JSON.parse(delta), expected);
  assert.deepEqual(JSON.parse(parsed.find((event) => event.type === 'response.function_call_arguments.done').arguments), expected);
  assert.deepEqual(JSON.parse(parsed.find((event) => event.type === 'response.output_item.done').item.arguments), expected);
  const terminal = parsed.find((event) => event.type === 'response.completed');
  assert.deepEqual(JSON.parse(terminal.response.output[0].arguments), expected);
  assert.equal(terminal.response.output[1].encrypted_content, 'old-child-A-opaque-encrypted');
  assert.deepEqual(raw, original);
});

test('Responses 原始副本不一致或出现未知目标参数事件时不能自动修正', () => {
  assert.throws(() => portable(makeResponses({ terminalArgs: '{"thread_id":"different"}' }), 'openai-responses'), errorCode('inconsistent_tool_arguments'));
  assert.throws(() => portable(makeResponses({ unknownEvent: true }), 'openai-responses'), errorCode('unsupported_tool_event'));
});

test('Responses 参数保留每份副本的字段顺序、空白和未修改词法，后续 history 可严格匹配', () => {
  const deltaArgs = '{\n "thread_id" : "old-child-A", "note":"old-child-A", "n":9007199254740993,"nested":[{"text":"\\u4e2d", "v":1.00}]\n}';
  const terminalArgs = '{"nested":[{"v":1.00,"text":"\\u4e2d"}],"n":9007199254740993,"note":"old-child-A","thread_id":"old-child-A"}';
  const item = { type: 'function_call', id: 'item-1', call_id: 'tool-resume', name: 'resume', arguments: terminalArgs };
  const raw = Buffer.from([
    frame({ type: 'response.output_item.added', output_index: 0, item: { ...item, arguments: '' } }),
    frame({ type: 'response.function_call_arguments.delta', output_index: 0, item_id: 'item-1', delta: deltaArgs.slice(0, 25) }),
    frame({ type: 'response.function_call_arguments.delta', output_index: 0, item_id: 'item-1', delta: deltaArgs.slice(25) }),
    frame({ type: 'response.function_call_arguments.done', output_index: 0, item_id: 'item-1', arguments: terminalArgs }),
    frame({ type: 'response.output_item.done', output_index: 0, item }),
    frame({ type: 'response.completed', response: { output: [item] } }),
  ].join(''));
  const original = Buffer.from(raw);
  const parsed = events(portable(raw, 'openai-responses').body);
  const actualDelta = parsed.filter((event) => event.type === 'response.function_call_arguments.delta').map((event) => event.delta).join('');
  const expectedDelta = deltaArgs.replace('"thread_id" : "old-child-A"', '"thread_id" : "live-child-A-with-a-longer-id"');
  const expectedTerminal = terminalArgs.replace('"thread_id":"old-child-A"', '"thread_id":"live-child-A-with-a-longer-id"');
  assert.equal(actualDelta, expectedDelta);
  assert.equal(parsed.find((event) => event.type === 'response.function_call_arguments.done').arguments, expectedTerminal);
  assert.equal(parsed.find((event) => event.type === 'response.output_item.done').item.arguments, expectedTerminal);
  assert.equal(parsed.find((event) => event.type === 'response.completed').response.output[0].arguments, expectedTerminal);
  assert.deepEqual(raw, original);
});

test('绑定表快照和派生规则元数据不能反向改变运行期绑定', () => {
  const registry = makeRegistry();
  const snapshot = registry.snapshot();
  snapshot[0].live = 'mutated';
  assert.equal(registry.rebindResponse({ thread_id: 'old-child-A' }, { owner: 'root-A', rules: [childRule] }).thread_id, 'live-child-A-with-a-longer-id');
  assert.deepEqual(registry.normalizeRequest({ thread_id: 'old-child-A' }, { owner: 'root-A', rules: [childRule], source: 'recorded' }), { thread_id: 'old-child-A' });
});

test('portable 只改声明字符串，保留未声明大整数及数值字面量', () => {
  const raw = Buffer.from([
    frame({ type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id: 'tool-resume', name: 'resume', input: {} } }),
    frame({ type: 'content_block_delta', index: 0, delta: { type: 'input_json_delta', partial_json: '{"thread_id":"old-child-A","n":9007199254740993,"decimal":1.00}' } }),
    frame({ type: 'content_block_stop', index: 0 }), frame({ type: 'message_stop' }),
  ].join(''));
  const args = events(portable(raw).body).filter((event) => event.type === 'content_block_delta').map((event) => event.delta.partial_json).join('');
  assert.ok(args.includes('9007199254740993'), '未声明大整数不得被 JS 浮点舍入');
  assert.ok(args.includes('1.00'), '未声明数值字面量应保持原样');
  const inline = Buffer.from('event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-resume","name":"resume","input":{"thread_id":"old-child-A","n":9007199254740993}}}\n\n' +
    frame({ type: 'content_block_stop', index: 0 }) + frame({ type: 'message_stop' }));
  assert.ok(portable(inline).body.includes('9007199254740993'));
});

test('工具参数重复 key 必须拒绝，不能在派生过程中把歧义修正成成功', () => {
  const raw = Buffer.from([
    frame({ type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id: 'tool-resume', name: 'resume', input: {} } }),
    frame({ type: 'content_block_delta', index: 0, delta: { type: 'input_json_delta', partial_json: '{"thread_id":"wrong","thread_id":"old-child-A"}' } }),
    frame({ type: 'content_block_stop', index: 0 }), frame({ type: 'message_stop' }),
  ].join(''));
  assert.throws(() => portable(raw), errorCode('duplicate_tool_argument_key'));
});

test('同一 owner 的多个工作区别名不能折叠，但不同会话可显式共享目录', () => {
  const registry = makeRegistry();
  assert.throws(() => registry.declare({ owner: 'root-A', kind: 'path', alias: 'another', recorded: '/recorded/work', anchor: 'workspace-2' }), errorCode('binding_alias_collision'));
  registry.declare({ owner: 'root-B', kind: 'path', alias: 'shared', recorded: '/recorded/work', anchor: 'shared-workspace' });
  registry.bind({ owner: 'root-B', kind: 'path', alias: 'shared', live: '/new/run/workspace', anchor: 'shared-workspace' });
  assert.equal(registry.snapshot().at(-1).live, '/new/run/workspace');
});
