import assert from 'node:assert/strict';
import test from 'node:test';
import { ReplayRun } from './run-state.mjs';
import { canonicalBody, identityKey } from './matcher.mjs';
import { createBindingRegistry } from './bindings.mjs';
import http from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { startReplayProxy, contextHeaders } from './proxy.mjs';
import { loadCassette } from './cassette.mjs';

function identity(owner = 'A', attempt = 1) {
  return { owner, execution: 'user-input-1', call: 'reason:1', purpose: 'reason', attempt };
}

function entries(specs) {
  return new Map(specs.map(([id, kind, size]) => [identityKey(id), {
    identity: id, request: { method: 'POST', target: 'https://provider.test/v1/messages', headers: {}, canonicalBody: '{}' },
    response: { status: 200, headers: {}, size, terminal: { kind, atMs: 100 } },
  }]));
}

async function bounded(work, timeout = 2000) {
  let timer;
  try {
    return await Promise.race([work, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('操作没有在期限内结束')), timeout);
    })]);
  } finally { clearTimeout(timer); }
}

async function makeEmptyRecorder(t, options = {}) {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'peri-abort-adversarial-'));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const proxy = await startReplayProxy({ mode: 'record', cassetteDir: path.join(temporary, 'case'), ...options });
  t.after(() => bounded(proxy.close()));
  return proxy;
}

test('录制中途取消不允许零字节提前取消冒充同一场景', () => {
  const id = identity();
  const run = new ReplayRun('abort-prefix', 'replay', entries([[id, 'client_abort', 100]]));
  const call = run.reserve(id);
  run.start(call);
  run.finish(call, 'client_abort');
  assert.throws(() => run.complete(), '仅 terminal.kind 相同不能判定中途取消场景通过');
});

test('完整响应没有发送完录制字节时不能 complete 假绿', () => {
  const id = identity();
  const run = new ReplayRun('short-response', 'replay', entries([[id, 'complete', 100]]));
  const call = run.reserve(id);
  run.start(call);
  run.progress(call, 1);
  run.finish(call, 'complete');
  assert.throws(() => run.complete(), '必须核验完整响应字节边界');
});

test('同一 logical call 的第二次 attempt 不能先于第一次领取', () => {
  const first = identity('A', 1);
  const second = identity('A', 2);
  const run = new ReplayRun('attempt-order', 'replay', entries([[first, 'upstream_error', 0], [second, 'complete', 0]]));
  assert.throws(() => run.reserve(second), '重试依赖属于同一个 call，不能当成任意并发请求');
});

test('同一 logical call 的 attempt 不能重叠，其他 owner 可独立推进', () => {
  const first = identity('A', 1);
  const second = identity('A', 2);
  const other = identity('B', 1);
  const run = new ReplayRun('attempt-overlap', 'replay', entries([[first, 'upstream_error', 0], [second, 'complete', 0], [other, 'complete', 0]]));
  const a = run.reserve(first);
  run.start(a);
  const b = run.reserve(other);
  run.start(b);
  run.finish(b, 'complete');
  assert.throws(() => run.reserve(second), '前一次 attempt 未结束时不能开启下一次');
});

test('跨 owner 反序完成及相同 cassette 跨 run 重放都保持独立', () => {
  const a = identity('A');
  const b = identity('B');
  const cassette = entries([[a, 'complete', 2], [b, 'complete', 3]]);
  const first = new ReplayRun('one', 'replay', cassette);
  const second = new ReplayRun('two', 'replay', cassette);
  for (const [run, order] of [[first, [[b, 3], [a, 2]]], [second, [[a, 2], [b, 3]]]]) {
    for (const [id, size] of order) {
      const call = run.reserve(id);
      run.start(call);
      run.progress(call, size);
      run.finish(call, 'complete');
      assert.throws(() => run.reserve(id), '同一 attempt 不可重复消费');
    }
    assert.equal(run.complete().passed, true);
  }
});

test('显式绑定不能让未声明的大整数或工具错误结果在 matcher 中等价', () => {
  const registry = createBindingRegistry({ runId: 'r', declarations: [
    { owner: 'A', kind: 'thread', alias: 'child', recorded: 'old', anchor: 'parent/tool' },
  ] });
  registry.bind({ owner: 'A', kind: 'thread', alias: 'child', live: 'new', anchor: 'parent/tool' });
  const options = { bindings: registry, owner: 'A', rules: [{ path: '/thread', kind: 'thread', alias: 'child' }] };
  const good = canonicalBody(Buffer.from('{"thread":"new","counter":9007199254740993,"is_error":false}'), options);
  const badNumber = canonicalBody(Buffer.from('{"thread":"new","counter":9007199254740992,"is_error":false}'), options);
  const badTool = canonicalBody(Buffer.from('{"thread":"new","counter":9007199254740993,"is_error":true}'), options);
  assert.notEqual(good, badNumber);
  assert.notEqual(good, badTool);
  assert.ok(good.includes('9007199254740993'));
});

test('真实 HTTP 录制后关闭上游，相同请求反序 portable 重放到正确子会话', async (t) => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'peri-portable-adversarial-'));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const cassetteDir = path.join(temporary, 'case');
  const target = 'https://fixture.test/v1/messages';
  const ids = [identity('A'), identity('B')];
  const rawResponses = [];
  const frame = (data) => `event: ${data.type}\ndata: ${JSON.stringify(data)}\n\n`;
  for (const owner of ['A', 'B']) {
    rawResponses.push([
      frame({ type: 'message_start', message: { id: `m-${owner}`, type: 'message', role: 'assistant', content: [] } }),
      frame({ type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id: 'tool-resume', name: 'resume', input: {} } }),
      frame({ type: 'content_block_delta', index: 0, delta: { type: 'input_json_delta', partial_json: `{"thread_id":"recorded-${owner}"}` } }),
      frame({ type: 'content_block_stop', index: 0 }),
      frame({ type: 'message_delta', delta: { stop_reason: 'tool_use' } }),
      frame({ type: 'message_stop' }),
    ].join(''));
  }
  let upstreamCalls = 0;
  let leakedContext = false;
  const upstream = http.createServer(async (request, response) => {
    for await (const _ of request) { /* 消费真实请求 body。 */ }
    leakedContext ||= request.headers['x-peri-replay-context'] !== undefined;
    const body = rawResponses[upstreamCalls++];
    response.writeHead(200, { 'content-type': 'text/event-stream', 'content-length': String(Buffer.byteLength(body)) });
    response.end(body);
  });
  await new Promise((resolve) => upstream.listen(0, '127.0.0.1', resolve));
  t.after(() => { upstream.closeAllConnections(); upstream.close(); });
  const recorder = await startReplayProxy({ mode: 'record', cassetteDir, upstreams: [
    { logicalOrigin: 'https://fixture.test', transportOrigin: `http://127.0.0.1:${upstream.address().port}` },
  ] });
  t.after(() => recorder.close());
  recorder.createRun('record');
  const ask = async (proxy, run, id) => {
    const response = await fetch(`${proxy.url}/v1/messages`, { method: 'POST', headers: {
      'content-type': 'application/json', ...contextHeaders({ run, ...id }, target),
    }, body: '{"model":"fixture","messages":[{"role":"user","content":"resume"}]}' });
    const body = await response.text();
    assert.equal(response.status, 200, body);
    if (response.headers.has('content-length')) assert.equal(Number(response.headers.get('content-length')), Buffer.byteLength(body));
    return body;
  };
  await ask(recorder, 'record', ids[0]);
  await ask(recorder, 'record', ids[1]);
  await recorder.waitFor('record', (state) => state.calls.length === 2 && state.active === 0);
  assert.equal(recorder.completeRun('record').passed, true);
  await recorder.seal();
  await recorder.close();
  await new Promise((resolve) => { upstream.closeAllConnections(); upstream.close(resolve); });
  const cassette = await loadCassette(cassetteDir);
  const originalManifest = await readFile(path.join(cassetteDir, 'manifest.json'));
  const registry = createBindingRegistry({ runId: 'live', declarations: ids.map((id) => ({
    owner: id.owner, kind: 'thread', alias: 'child', recorded: `recorded-${id.owner}`, anchor: `parent-${id.owner}/tool-spawn`,
  })) });
  for (const id of [...ids].reverse()) registry.bind({ owner: id.owner, kind: 'thread', alias: 'child', live: `live-${id.owner}-different-length`, anchor: `parent-${id.owner}/tool-spawn` });
  const replayer = await startReplayProxy({ mode: 'replay', cassetteDir });
  t.after(() => replayer.close());
  replayer.createRun('live', { bindings: registry, portableResponses: ids.map((id) => ({
    identity: id, protocol: 'anthropic', rulesVersion: 'adversarial-v1', fragmentPlan: { kind: 'utf8', maxBytes: 9 },
    tools: [{ toolCallId: 'tool-resume', rules: [{ path: '/thread_id', kind: 'thread', alias: 'child' }] }],
  })) });
  const [b, a] = await Promise.all([ask(replayer, 'live', ids[1]), ask(replayer, 'live', ids[0])]);
  const argumentText = (body) => body.split('\n\n').filter(Boolean).map((block) => JSON.parse(block.split('\n').find((line) => line.startsWith('data:')).slice(5)))
    .filter((event) => event.delta?.type === 'input_json_delta').map((event) => event.delta.partial_json).join('');
  assert.equal(JSON.parse(argumentText(a)).thread_id, 'live-A-different-length');
  assert.equal(JSON.parse(argumentText(b)).thread_id, 'live-B-different-length');
  await replayer.waitFor('live', (state) => state.calls.length === 2 && state.active === 0);
  const result = replayer.completeRun('live');
  assert.equal(result.passed, true);
  assert.equal(result.upstreamCalls, 0);
  assert.equal(upstreamCalls, 2);
  assert.equal(leakedContext, false);
  assert.ok(result.calls.every((call) => call.derivedFixture?.mode === 'portable'));
  assert.deepEqual(await readFile(path.join(cassetteDir, 'manifest.json')), originalManifest);
  for (const entry of cassette.entries.values()) {
    const index = entry.identity.owner === 'A' ? 0 : 1;
    assert.equal(await readFile(path.join(cassetteDir, entry.response.file), 'utf8'), rawResponses[index]);
  }
});

test('身份解析未返回也受请求期限约束，close 不得等待永不完成的解析器', async (t) => {
  const proxy = await makeEmptyRecorder(t, { requestTimeoutMs: 30 });
  proxy.createRun('r', { resolveContext: () => new Promise(() => {}) });
  const response = await bounded(fetch(`${proxy.url}/v1/messages`, {
    method: 'POST', headers: contextHeaders({ run: 'r', ...identity() }, 'https://fixture.test/v1/messages'), body: '{}',
  }));
  assert.equal(response.status, 409);
  assert.equal((await response.json()).error, 'request_timeout');
  assert.ok(proxy.snapshot('r').errors.some((error) => error.code === 'request_timeout'));
  assert.throws(() => proxy.completeRun('r'));
  await bounded(proxy.close());
});

test('请求 body 未发送完时期限仍生效，半开客户端不能阻塞 close', async (t) => {
  const proxy = await makeEmptyRecorder(t, { requestTimeoutMs: 30 });
  proxy.createRun('r');
  const request = http.request(`${proxy.url}/v1/messages`, { method: 'POST', headers: {
    ...contextHeaders({ run: 'r', ...identity() }, 'https://fixture.test/v1/messages'), 'content-length': '100',
  } }, (response) => response.resume());
  request.on('error', () => {});
  t.after(() => request.destroy());
  request.write('{');
  await bounded(proxy.waitFor('r', (state) => state.errors.some((error) => error.code === 'request_timeout')));
  assert.equal(proxy.snapshot('r').upstreamCalls, 0);
  assert.throws(() => proxy.completeRun('r'));
  await bounded(proxy.close());
});

test('runner 关闭挂起的身份解析会主动取消，不伪装成产品 client_abort', async (t) => {
  const proxy = await makeEmptyRecorder(t, { requestTimeoutMs: 60000 });
  let entered;
  const enteredPromise = new Promise((resolve) => { entered = resolve; });
  let resolverSignal;
  proxy.createRun('r', { resolveContext: (_, __, signal) => {
    resolverSignal = signal;
    entered();
    return new Promise(() => {});
  } });
  const request = fetch(`${proxy.url}/v1/messages`, {
    method: 'POST', headers: contextHeaders({ run: 'r', ...identity() }, 'https://fixture.test/v1/messages'), body: '{}',
  }).catch(() => undefined);
  await bounded(enteredPromise);
  await bounded(proxy.close());
  await bounded(request);
  assert.equal(resolverSignal.aborted, true);
  assert.equal(resolverSignal.reason.code, 'proxy_shutdown');
  assert.ok(proxy.snapshot('r').errors.some((error) => error.code === 'proxy_shutdown'));
});

test('run 不能借用其他 run 的动态绑定表', async (t) => {
  const proxy = await makeEmptyRecorder(t);
  assert.throws(() => proxy.createRun('A', { bindings: createBindingRegistry({ runId: 'B' }) }));
});
