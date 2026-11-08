import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createRequestBindingAdapter, discoverBindingRules } from './request-bindings.mjs';
import { canonicalBody, identityKey } from './matcher.mjs';
import { contextHeaders, startReplayProxy } from './proxy.mjs';

const identity = { owner: 'root', execution: 'turn-1', call: 'reason:1', attempt: 1, purpose: 'reason' };
const anchor = (alias) => `scenario/${alias}`;
const worldValues = (root, session, today = '2026-11-07') => ({
  root: ['path', root], home: ['path', `${root}/home`], work: ['path', `${root}/work`],
  session: ['session', session], today: ['date', today],
});
function bindWorld(adapter, values) {
  for (const [alias, [kind, live]] of Object.entries(values)) adapter.declareAndBind({ kind, alias, live, anchor: anchor(alias) });
}
function body(root, session, today = '2026-11-07') {
  return { model: 'fixture-model', system: [{ type: 'text', text:
    `strict instruction\n<env>\nWorking directory: ${root}/work\nToday's date: ${today}\nSession ID: ${session}\n</env>` }],
  messages: [{ role: 'user', content: 'A fixed business date 2026-11-07 must not vary.' }],
  tools: [{ name: 'Write' }] };
}

test('跨目录和会话只转换模板中的显式引用，周围 prompt 与用户日期仍严格匹配', () => {
  const recording = createRequestBindingAdapter({ mode: 'record', runId: 'record' });
  bindWorld(recording, worldValues('/record/root', 'session-recorded'));
  const original = Buffer.from(JSON.stringify(body('/record/root', 'session-recorded')));
  recording.onRequest({ context: identity, request: { body: original } });
  const rules = recording.requestRules(identity);
  assert.deepEqual(rules.map((rule) => rule.path), ['/system/0/text']);
  assert.equal(rules[0].segments.filter((part) => part?.alias === 'work').length, 1, '最长路径别名胜出');
  assert.ok(rules[0].segments.some((part) => part?.alias === 'today'));
  const replaying = createRequestBindingAdapter({ mode: 'replay', runId: 'replay', declarations: recording.snapshotDeclarations() });
  bindWorld(replaying, worldValues('/another/runtime/world', 'new-longer-session', '2026-11-08'));
  const actual = body('/another/runtime/world', 'new-longer-session', '2026-11-08');
  const canonical = (value, registry) => canonicalBody(Buffer.from(JSON.stringify(value)), { bindings: registry, owner: 'world', rules });
  assert.equal(canonical(actual, replaying.registry), canonical(JSON.parse(original), recording.registry));
  assert.throws(() => canonical({ ...actual, system: [{ type: 'text', text: actual.system[0].text.replace('strict instruction', 'changed instruction') }] }, replaying.registry), { code: 'binding_value_mismatch' });
  actual.messages[0].content = actual.messages[0].content.replace('2026-11-07', '2026-11-08');
  assert.notEqual(canonical(actual, replaying.registry), canonical(JSON.parse(original), recording.registry));
  assert.throws(() => replaying.declareAndBind({ kind: 'path', alias: 'unknown', live: '/unknown', anchor: 'unknown' }), { code: 'binding_not_declared' });
});

test('自动发现遵守路径及 ID 边界，不能把相邻前缀、穿越、opaque 或用户日期当变量', () => {
  const adapter = createRequestBindingAdapter({ mode: 'record', runId: 'boundaries' });
  bindWorld(adapter, worldValues('/record/root', 'session-recorded'));
  const value = { good: 'cwd=/record/root/work/file.ts; session=session-recorded',
    adjacent: '/record/rootish/work', traversal: '/record/root/work/../outside',
    embedded: 'prefixsession-recorded', task: 'bg-session-recorded',
    thinking: { type: 'thinking', text: '/record/root/work', signature: 'session-recorded' },
    user: "Today's date: 2026-11-07", system: [{ type: 'text', text: "Today's date: 2026-11-07" }] };
  const rules = discoverBindingRules(value, { registry: adapter.registry });
  assert.deepEqual(rules.map((rule) => rule.path), ['/good']);
  const normalized = adapter.registry.normalizeRequest(value, { owner: 'world', rules });
  assert.deepEqual(normalized, value);
  assert.throws(() => adapter.declareAndBind({ kind: 'date', alias: 'bad-date', live: '2026-02-30', anchor: 'bad' }), { code: 'invalid_binding_date' });
});

test('平台与 OS 版本只绑定系统环境段的带标签行，用户消息中的同形文本仍严格匹配', () => {
  const envValues = (root, session, platform, osVersion) => ({
    ...worldValues(root, session), platform: ['platform', platform], os_version: ['os_version', osVersion],
  });
  const recording = createRequestBindingAdapter({ mode: 'record', runId: 'platform-record' });
  bindWorld(recording, envValues('/record/root', 'session-recorded', 'linux', 'Ubuntu 24.04.3 LTS'));
  const system = (root, platform, osVersion) => `strict instruction\n<env>\nWorking directory: ${root}/work\n` +
    `Platform: ${platform}\nOS Version: ${osVersion}\nToday's date: 2026-11-07\n</env>`;
  const original = { model: 'fixture-model', system: [{ type: 'text', text: system('/record/root', 'linux', 'Ubuntu 24.04.3 LTS') }],
    messages: [{ role: 'user', content: 'Platform: linux\nOS Version: Ubuntu 24.04.3 LTS' }] };
  recording.onRequest({ context: identity, request: { body: Buffer.from(JSON.stringify(original)) } });
  const rules = recording.requestRules(identity);
  assert.deepEqual(rules.map((rule) => rule.path), ['/system/0/text']);
  assert.ok(rules[0].segments.some((part) => part?.alias === 'platform'));
  assert.ok(rules[0].segments.some((part) => part?.alias === 'os_version'));

  const replaying = createRequestBindingAdapter({ mode: 'replay', runId: 'platform-replay', declarations: recording.snapshotDeclarations() });
  bindWorld(replaying, envValues('/runtime/root', 'new-session', 'macos', 'macOS 26.5.1'));
  const actual = structuredClone(original);
  actual.system[0].text = system('/runtime/root', 'macos', 'macOS 26.5.1');
  const canonical = (value, registry) => canonicalBody(Buffer.from(JSON.stringify(value)), { bindings: registry, owner: 'world', rules });
  assert.equal(canonical(actual, replaying.registry), canonical(original, recording.registry));
  actual.messages[0].content = 'Platform: macos\nOS Version: macOS 26.5.1';
  assert.notEqual(canonical(actual, replaying.registry), canonical(original, recording.registry));
  assert.deepEqual(discoverBindingRules({ system: [{ type: 'text', text: 'Platform: linux\nOS Version: Ubuntu 24.04.3 LTS' }] },
    { registry: recording.registry }).map((rule) => rule.path), [], '没有 <env> 段时不得推断可忽略字段');
});

test('typed 模板不能借跨 owner、未知引用或额外对象字段获取宽松匹配', () => {
  const adapter = createRequestBindingAdapter({ mode: 'record', runId: 'strict' });
  bindWorld(adapter, worldValues('/record/root', 'session-recorded'));
  const normalize = (segments, owner = 'world') => adapter.registry.normalizeRequest({ value: 'session-recorded' }, {
    owner, rules: [{ path: '/value', mode: 'typed-template', segments }],
  });
  assert.throws(() => normalize([{ kind: 'session', alias: 'session', owner: 'world' }]), { code: 'invalid_binding_template' });
  assert.throws(() => normalize([{ kind: 'session', alias: 'unknown' }]), { code: 'binding_not_declared' });
  assert.throws(() => normalize([{ kind: 'session', alias: 'session' }], 'other'), { code: 'binding_not_declared' });
});

test('封存一次后关闭上游，两个并行 run 以各自新目录重放 typed 工具参数且原件不变', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'peri-bindings-'));
  const cassetteDir = path.join(directory, 'cassette');
  const sourceRoot = '/record/original';
  const event = (type, data) => `event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`;
  const raw = event('content_block_start', { index: 0, content_block: {
    type: 'tool_use', id: 'write-result', name: 'Write', input: { file_path: `${sourceRoot}/work/result.txt`, content: 'strict-content' },
  } }) + event('content_block_stop', { index: 0 }) + event('message_stop', {});
  const upstream = createServer((request, response) => {
    request.resume();
    request.on('end', () => { response.writeHead(200, { 'content-type': 'text/event-stream' }); response.end(raw); });
  });
  await new Promise((resolve) => upstream.listen(0, '127.0.0.1', resolve));
  let proxy;
  const closeUpstream = () => new Promise((resolve) => { upstream.closeAllConnections(); upstream.close(resolve); });
  const send = async (id, root, session) => {
    const response = await fetch(`${proxy.url}/v1/messages`, { method: 'POST', headers: {
      ...contextHeaders({ run: id, ...identity }, 'http://fixture.test/v1/messages'), 'content-type': 'application/json',
    }, body: JSON.stringify(body(root, session)) });
    assert.equal(response.status, 200);
    return response.text();
  };
  try {
    const recording = createRequestBindingAdapter({ mode: 'record', runId: 'record' });
    bindWorld(recording, worldValues(sourceRoot, 'record-session'));
    proxy = await startReplayProxy({ mode: 'record', cassetteDir, upstreams: [{
      logicalOrigin: 'http://fixture.test', transportOrigin: `http://127.0.0.1:${upstream.address().port}`,
    }] });
    proxy.createRun('record', { bindings: recording.registry, bindingOwnerFor: recording.bindingOwnerFor,
      onRequest: recording.onRequest, requestRules: recording.requestRules });
    assert.equal(await send('record', sourceRoot, 'record-session'), raw);
    await proxy.waitFor('record', (state) => state.active === 0);
    proxy.completeRun('record');
    await proxy.seal();
    const plans = await recording.responsePlans(cassetteDir);
    assert.equal(plans.length, 1);
    assert.equal(plans[0].tools[0].rules[0].path, '/file_path');
    const before = await readFile(path.join(cassetteDir, 'manifest.json'));
    await proxy.close();
    await closeUpstream();
    proxy = await startReplayProxy({ mode: 'replay', cassetteDir });
    for (const id of ['A', 'B']) {
      const adapter = createRequestBindingAdapter({ mode: 'replay', runId: id, declarations: recording.snapshotDeclarations() });
      bindWorld(adapter, worldValues(`/runtime/${id}`, `${id}-session`));
      proxy.createRun(id, { bindings: adapter.registry, bindingOwnerFor: adapter.bindingOwnerFor, portableResponses: plans });
    }
    const results = await Promise.all(['B', 'A'].map((id) => send(id, `/runtime/${id}`, `${id}-session`)));
    for (const [index, id] of ['B', 'A'].entries()) {
      assert.ok(results[index].includes(`/runtime/${id}/work/result.txt`));
      assert.ok(!results[index].includes(sourceRoot));
      assert.ok(results[index].includes('strict-content'));
      await proxy.waitFor(id, (state) => state.active === 0);
      const summary = proxy.completeRun(id);
      assert.equal(summary.upstreamCalls, 0);
      assert.equal(summary.calls.length, 1);
      assert.equal(identityKey(summary.calls[0].identity), identityKey(identity));
      assert.ok(summary.calls[0].derivedFixture.sourceSha256);
    }
    assert.deepEqual(await readFile(path.join(cassetteDir, 'manifest.json')), before);
  } finally {
    await proxy?.close();
    await closeUpstream();
    await rm(directory, { recursive: true, force: true });
  }
});
