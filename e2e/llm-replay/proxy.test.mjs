import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { startReplayProxy, contextHeaders } from './proxy.mjs';
import { canonicalBody, identityKey, logicalTarget } from './matcher.mjs';
import { loadCassette } from './cassette.mjs';

const logical = 'http://fixture.invalid';
const endpoint = `${logical}/v1/messages`;
const body = { model: 'test-model', messages: [{ role: 'user', content: 'same question' }] };
const identity = (owner = 'A', call = '1', attempt = 1) => ({ owner, execution: 'turn-1', call, attempt, purpose: 'reason' });
const sse = (text) => `event: content_block_delta\ndata: ${JSON.stringify({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } })}\n\nevent: message_stop\ndata: {"type":"message_stop"}\n\n`;

async function directory(t) {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'peri-proxy-test-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  return path.join(dir, 'cassette');
}

async function listen(t, handler) {
  const server = http.createServer(handler);
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const close = async () => {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  };
  t.after(close);
  return { url: `http://127.0.0.1:${server.address().port}`, close };
}

function request(proxy, run, id, value = body, extra = {}) {
  return fetch(`${proxy.url}/v1/messages`, {
    method: 'POST', headers: { 'content-type': 'application/json', ...contextHeaders({ run, ...id }, endpoint), ...extra.headers },
    body: typeof value === 'string' ? value : JSON.stringify(value), signal: extra.signal,
  });
}

async function text(proxy, run, id, value = body) {
  const response = await request(proxy, run, id, value);
  return { status: response.status, text: await response.text() };
}

async function settled(proxy, run, count) {
  await proxy.waitFor(run, (state) => state.calls.length === count && state.active === 0);
}

async function recordPair(t) {
  const cassetteDir = await directory(t);
  let calls = 0;
  const upstream = await listen(t, async (req, res) => {
    for await (const _ of req) { /* 消费真实请求。 */ }
    assert.equal(req.headers['x-peri-replay-context'], undefined);
    assert.equal(req.headers['x-peri-replay-target'], undefined);
    if (calls === 0) assert.equal(req.headers.authorization, 'Bearer fixture-private');
    calls++;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end(sse(`answer-${calls}`));
  });
  const recorder = await startReplayProxy({ mode: 'record', cassetteDir, upstreams: [{ logicalOrigin: logical, transportOrigin: upstream.url }] });
  t.after(() => recorder.close());
  recorder.createRun('record');
  const firstResponse = await request(recorder, 'record', identity('A'), body, { headers: { authorization: 'Bearer fixture-private' } });
  assert.match(await firstResponse.text(), /answer-1/);
  assert.match((await text(recorder, 'record', identity('B'))).text, /answer-2/);
  await settled(recorder, 'record', 2);
  recorder.completeRun('record');
  await recorder.seal();
  assert.doesNotMatch(await readFile(path.join(cassetteDir, 'manifest.json'), 'utf8'), /fixture-private|authorization/);
  await recorder.close();
  await upstream.close();
  return { cassetteDir, calls: () => calls };
}

test('同正文双 owner 反序重放，两个并行 run 独立消费且上游已关闭', async (t) => {
  const fixture = await recordPair(t);
  const replay = await startReplayProxy({ mode: 'replay', cassetteDir: fixture.cassetteDir });
  t.after(() => replay.close());
  replay.createRun('one'); replay.createRun('two');
  const results = await Promise.all([
    text(replay, 'one', identity('B')), text(replay, 'two', identity('A')),
    text(replay, 'one', identity('A')), text(replay, 'two', identity('B')),
  ]);
  assert.match(results[0].text, /answer-2/);
  assert.match(results[1].text, /answer-1/);
  assert.match(results[2].text, /answer-1/);
  assert.match(results[3].text, /answer-2/);
  for (const run of ['one', 'two']) {
    await settled(replay, run, 2);
    const summary = replay.completeRun(run);
    assert.equal(summary.passed, true);
    assert.equal(summary.upstreamCalls, 0);
    assert.equal(summary.calls.length, 2);
  }
  assert.equal(fixture.calls(), 2);
  const cassette = await loadCassette(fixture.cassetteDir);
  assert.equal([...cassette.entries.values()][0].response.protocolFacts.terminalMarker, 'seen');
});

test('缺失、额外、重复或正文漂移的请求不能通过完整验收', async (t) => {
  const { cassetteDir } = await recordPair(t);
  const replay = await startReplayProxy({ mode: 'replay', cassetteDir });
  t.after(() => replay.close());
  replay.createRun('missing');
  await text(replay, 'missing', identity('A'));
  await settled(replay, 'missing', 1);
  assert.throws(() => replay.completeRun('missing'), { code: 'run_incomplete' });
  const driftRun = replay.createRun('drift', { gates: [{ identity: identity('A'), afterBytes: 0 }] });
  const drift = await text(replay, 'drift', identity('A'), { ...body, model: 'wrong-model' });
  assert.equal(drift.status, 409);
  assert.equal(JSON.parse(drift.text).error, 'request_mismatch');
  await assert.rejects(driftRun.waitReached(identity('A')), { code: 'run_failed' });
  assert.throws(() => replay.completeRun('drift'), { code: 'run_incomplete' });
  for (const kind of ['extra', 'duplicate']) {
    replay.createRun(kind);
    await text(replay, kind, identity('A')); await text(replay, kind, identity('B'));
    const response = await text(replay, kind, identity('A', kind === 'extra' ? '2' : '1'));
    assert.equal(response.status, 409);
    assert.equal(JSON.parse(response.text).error, kind === 'extra' ? 'unmatched_request' : 'duplicate_identity');
    assert.throws(() => replay.completeRun(kind), { code: 'run_incomplete' });
  }
});

test('请求 JSON 键序可变，但重复键、大整数及数组顺序保持严格', () => {
  assert.equal(canonicalBody(Buffer.from('{"b":2,"a":1}')), canonicalBody(Buffer.from('{"a":1,"b":2}')));
  assert.notEqual(canonicalBody(Buffer.from('{"n":9007199254740992}')), canonicalBody(Buffer.from('{"n":9007199254740993}')));
  assert.notEqual(canonicalBody(Buffer.from('[1,2]')), canonicalBody(Buffer.from('[2,1]')));
  assert.throws(() => canonicalBody(Buffer.from('{"a":1,"a":2}')), { code: 'duplicate_json_key' });
  assert.throws(() => logicalTarget('https://provider.test/v1/messages?api_key=sensitive'), { code: 'credential_in_target' });
});

test('控制门由真实客户端取消，响应消费不回卷，提前取消必须失败', async (t) => {
  const cassetteDir = await directory(t);
  const prefix = 'event: content_block_delta\ndata: {"type":"content_block_delta","delta":{"type":"text_delta","text":"prefix"}}\n\n';
  const upstream = await listen(t, async (req, res) => {
    for await (const _ of req) { }
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end(prefix + sse('suffix'));
  });
  const id = identity();
  const recorder = await startReplayProxy({ mode: 'record', cassetteDir, upstreams: [{ logicalOrigin: logical, transportOrigin: upstream.url }] });
  t.after(() => recorder.close());
  const recordRun = recorder.createRun('record', { gates: [{ identity: id, afterBytes: Buffer.byteLength(prefix) }] });
  const controller = new AbortController();
  const response = await request(recorder, 'record', id, body, { signal: controller.signal });
  const reader = response.body.getReader();
  const first = await reader.read();
  assert.equal(Buffer.from(first.value).toString(), prefix);
  await recordRun.waitReached(id);
  controller.abort();
  await reader.read().catch(() => {});
  await recorder.waitFor('record', (state) => state.calls[0]?.terminal === 'client_abort');
  await settled(recorder, 'record', 1);
  recorder.completeRun('record'); await recorder.seal();
  await recorder.close(); await upstream.close();

  const replay = await startReplayProxy({ mode: 'replay', cassetteDir });
  t.after(() => replay.close());
  const run = replay.createRun('good', { gates: [{ identity: id, afterBytes: Buffer.byteLength(prefix) }] });
  const cancel = new AbortController();
  const played = await request(replay, 'good', id, body, { signal: cancel.signal });
  const replayReader = played.body.getReader();
  assert.equal(Buffer.from((await replayReader.read()).value).toString(), prefix);
  await run.waitReached(id);
  cancel.abort(); await replayReader.read().catch(() => {});
  await replay.waitFor('good', (state) => state.calls[0]?.terminal === 'client_abort');
  assert.equal(replay.completeRun('good').passed, true);
  const duplicate = await text(replay, 'good', id);
  assert.equal(duplicate.status, 409);
  assert.throws(() => replay.completeRun('good'), { code: 'run_incomplete' });

  const earlyRun = replay.createRun('early', { gates: [{ identity: id, afterBytes: 0 }] });
  const earlyCancel = new AbortController();
  const early = await request(replay, 'early', id, body, { signal: earlyCancel.signal });
  await earlyRun.waitReached(id);
  earlyCancel.abort(); await early.text().catch(() => {});
  await settled(replay, 'early', 1);
  assert.equal(replay.snapshot('early').errors[0].code, 'response_prefix_mismatch');
  assert.throws(() => replay.completeRun('early'), { code: 'run_incomplete' });
});

test('上游截断被原样重放，不伪造协议终结标记', async (t) => {
  const cassetteDir = await directory(t);
  const prefix = 'event: content_block_delta\ndata: {"delta":{"type":"text_delta","text":"partial"}}\n\n';
  const upstream = await listen(t, async (req, res) => {
    for await (const _ of req) { }
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.flushHeaders();
    res.write(prefix, () => res.destroy());
  });
  const recorder = await startReplayProxy({ mode: 'record', cassetteDir, upstreams: [{ logicalOrigin: logical, transportOrigin: upstream.url }] });
  t.after(() => recorder.close()); recorder.createRun('record');
  await assert.rejects(text(recorder, 'record', identity()));
  await settled(recorder, 'record', 1);
  assert.equal(recorder.snapshot('record').calls[0].terminal, 'upstream_abort');
  recorder.completeRun('record'); await recorder.seal();
  await recorder.close(); await upstream.close();
  const cassette = await loadCassette(cassetteDir);
  const entry = cassette.entries.get(identityKey(identity()));
  assert.equal(entry.response.protocolFacts.terminalMarker, 'not_seen');
  assert.equal((await readFile(path.join(cassetteDir, entry.response.file))).toString(), prefix);
  const replay = await startReplayProxy({ mode: 'replay', cassetteDir });
  t.after(() => replay.close()); replay.createRun('replay');
  await assert.rejects(text(replay, 'replay', identity()));
  await settled(replay, 'replay', 1);
  assert.equal(replay.snapshot('replay').calls[0].terminal, 'upstream_abort');
  assert.equal(replay.completeRun('replay').passed, true);
});

test('未封存及被篡改的 cassette 在服务启动前失败', async (t) => {
  const directoryName = await directory(t);
  await assert.rejects(startReplayProxy({ mode: 'replay', cassetteDir: directoryName }), { code: 'unsealed_cassette' });
  const { cassetteDir } = await recordPair(t);
  const cassette = await loadCassette(cassetteDir);
  const first = [...cassette.entries.values()][0];
  await writeFile(path.join(cassetteDir, first.response.file), 'corrupt');
  await assert.rejects(startReplayProxy({ mode: 'replay', cassetteDir }), { code: 'corrupt_cassette' });
});

test('父工具因果锚错误或缺失不能领取同名子调用', async (t) => {
  const cassetteDir = await directory(t);
  const parent = { owner: 'root', execution: 'turn-1', call: '1', tool_call: 'spawn-1', slot: 0 };
  const child = { ...identity('child'), parent };
  const upstream = await listen(t, async (req, res) => {
    for await (const _ of req) { }
    res.writeHead(200, { 'content-type': 'text/event-stream' }); res.end(sse('child'));
  });
  const recorder = await startReplayProxy({ mode: 'record', cassetteDir, upstreams: [{ logicalOrigin: logical, transportOrigin: upstream.url }] });
  t.after(() => recorder.close()); recorder.createRun('record');
  await text(recorder, 'record', child); await settled(recorder, 'record', 1);
  recorder.completeRun('record'); await recorder.seal(); await recorder.close(); await upstream.close();
  const replay = await startReplayProxy({ mode: 'replay', cassetteDir });
  t.after(() => replay.close()); replay.createRun('wrong'); replay.createRun('missing');
  assert.equal(JSON.parse((await text(replay, 'wrong', { ...child, parent: { ...parent, slot: 1 } })).text).error, 'parent_mismatch');
  assert.equal(JSON.parse((await text(replay, 'missing', identity('child'))).text).error, 'parent_mismatch');
  assert.throws(() => replay.completeRun('wrong'), { code: 'run_incomplete' });
});

test('同 run 内不同 owner 可有独立进程 generation，旧 generation 被拒绝', async (t) => {
  const { cassetteDir } = await recordPair(t);
  const replay = await startReplayProxy({ mode: 'replay', cassetteDir });
  t.after(() => replay.close());
  replay.createRun('processes', { generations: { A: 'process-a-2', B: 'process-b-1' } });
  assert.equal((await text(replay, 'processes', { ...identity('B'), generation: 'process-b-1' })).status, 200);
  assert.equal((await text(replay, 'processes', { ...identity('A'), generation: 'process-a-2' })).status, 200);
  await settled(replay, 'processes', 2);
  assert.equal(replay.completeRun('processes').passed, true);
  const run = replay.createRun('stale');
  run.setOwnerGeneration('A', 'process-a-2');
  const response = await text(replay, 'stale', { ...identity('A'), generation: 'process-a-1' });
  assert.equal(response.status, 409);
  assert.equal(JSON.parse(response.text).error, 'stale_generation');
  assert.throws(() => replay.completeRun('stale'), { code: 'run_incomplete' });
});
