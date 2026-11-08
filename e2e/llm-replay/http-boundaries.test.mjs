import assert from 'node:assert/strict';
import http from 'node:http';
import net from 'node:net';
import { mkdtemp, open, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { contextHeaders, startReplayProxy } from './proxy.mjs';
import { loadCassette, readResponseChunks } from './cassette.mjs';

const TARGET = 'http://fixture.test/v1/chat/completions';
const CALL = { owner: 'A', execution: 'turn-1', call: 'reason:1', purpose: 'reason', attempt: 1 };

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

async function listen(server) {
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  return `http://127.0.0.1:${server.address().port}`;
}

async function close(server) {
  if (!server) return;
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}

async function send(proxy, run, attempt = 1) {
  const response = await fetch(`${proxy.url}/v1/chat/completions`, {
    method: 'POST', headers: { ...contextHeaders({ run, ...CALL, attempt }, TARGET), 'content-type': 'application/json' },
    body: '{}', signal: AbortSignal.timeout(3000),
  });
  return { status: response.status, body: await response.text() };
}

test('响应已经交付后，磁盘封存等待不能拒绝同一调用的下一次 HTTP 重试', { timeout: 8000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-retry-durable-'));
  const blocked = deferred(), release = deferred();
  const rateBody = '{"error":"rate-limit-durable-boundary"}';
  const probe = await open(path.join(directory, 'probe'), 'w');
  const prototype = Object.getPrototypeOf(probe), originalSync = prototype.sync;
  await probe.close();
  // 隔离进程内的文件句柄注入可控磁盘等待；只挡指定响应的 sync，不依赖墙钟竞速。
  prototype.sync = async function () {
    if ((await this.stat()).size === Buffer.byteLength(rateBody)) {
      blocked.resolve();
      await release.promise;
    }
    return originalSync.call(this);
  };
  let count = 0;
  const upstream = http.createServer((request, response) => {
    request.resume();
    request.on('end', () => {
      count++;
      response.writeHead(count === 1 ? 429 : 200, { 'content-type': 'application/json' });
      response.end(count === 1 ? rateBody : '{"ok":true}');
    });
  });
  let proxy;
  try {
    const upstreamUrl = await listen(upstream);
    proxy = await startReplayProxy({ mode: 'record', cassetteDir: path.join(directory, 'cassette'),
      upstreams: [{ logicalOrigin: 'http://fixture.test', transportOrigin: upstreamUrl }] });
    proxy.createRun('record');
    assert.equal((await send(proxy, 'record')).status, 429);
    await blocked.promise;
    assert.throws(() => proxy.completeRun('record'), '磁盘仍在写入时不能封存');
    assert.equal((await send(proxy, 'record', 2)).status, 200, '已收到完整 429 的客户端有权发起重试');
    release.resolve();
    await proxy.waitFor('record', (state) => state.active === 0);
    assert.equal(proxy.completeRun('record').passed, true);
    await proxy.seal();
    assert.equal(count, 2);
  } finally {
    release.resolve();
    await proxy?.close();
    await close(upstream);
    prototype.sync = originalSync;
    await rm(directory, { recursive: true, force: true });
  }
});

test('请求体没有结束时，代理自己的 deadline 仍会失败并释放活动请求', { timeout: 5000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-request-deadline-'));
  const proxy = await startReplayProxy({ mode: 'record', cassetteDir: path.join(directory, 'cassette'), requestTimeoutMs: 30 });
  proxy.createRun('record');
  const url = new URL(proxy.url);
  const socket = net.createConnection({ host: url.hostname, port: Number(url.port) });
  socket.on('error', () => {});
  const received = deferred();
  let bytes = '';
  socket.on('data', (chunk) => { bytes += chunk; if (bytes.includes('request_timeout')) received.resolve(); });
  try {
    await new Promise((resolve) => socket.once('connect', resolve));
    const headers = contextHeaders({ run: 'record', ...CALL }, TARGET);
    socket.write(`POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n${Object.entries(headers).map(([name, value]) => `${name}: ${value}`).join('\r\n')}\r\n\r\n{}`);
    await proxy.waitFor('record', (state) => state.errors.some((error) => error.code === 'request_timeout'), { timeout: 1000 });
    await received.promise;
    assert.match(bytes, /^HTTP\/1\.1 409 /);
    assert.equal(proxy.snapshot('record').upstreamCalls, 0);
    assert.throws(() => proxy.completeRun('record'));
  } finally {
    socket.destroy();
    await proxy.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('身份解析回调不返回时，deadline 不会被悬空 Promise 绕过', { timeout: 5000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-context-deadline-'));
  const proxy = await startReplayProxy({ mode: 'record', cassetteDir: path.join(directory, 'cassette'), requestTimeoutMs: 30 });
  proxy.createRun('record', { resolveContext: () => new Promise(() => {}) });
  try {
    const response = await send(proxy, 'record');
    assert.equal(response.status, 409);
    assert.equal(JSON.parse(response.body).error, 'request_timeout');
    assert.equal(proxy.snapshot('record').upstreamCalls, 0);
    assert.throws(() => proxy.completeRun('record'));
  } finally {
    await proxy.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('取消时录制保留已观察而未提交的尾部，重放不会把它当成客户端前缀', { timeout: 8000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-cancel-cut-'));
  const cassetteDir = path.join(directory, 'cassette');
  const body = Buffer.from('data: {"choices":[{"delta":{"content":"unsubmitted"}}]}\n\ndata: [DONE]\n\n');
  const appended = deferred(), releaseAppend = deferred(), aborted = deferred();
  const probe = await open(path.join(directory, 'probe'), 'w');
  const prototype = Object.getPrototypeOf(probe), originalWrite = prototype.writeFile;
  await probe.close();
  prototype.writeFile = async function (bytes, ...options) {
    const result = await originalWrite.call(this, bytes, ...options);
    if (Buffer.isBuffer(bytes) && bytes.equals(body)) {
      appended.resolve();
      await releaseAppend.promise;
    }
    return result;
  };
  const upstream = http.createServer((request, response) => {
    request.resume();
    request.on('end', () => {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(body);
    });
  });
  const openStream = (proxy, run) => new Promise((resolve, reject) => {
    const request = http.request(`${proxy.url}/v1/chat/completions`, {
      method: 'POST', headers: contextHeaders({ run, ...CALL }, TARGET),
    }, (response) => {
      const chunks = [];
      response.on('data', (bytes) => chunks.push(bytes));
      response.on('error', () => {});
      resolve({ response, chunks });
    });
    request.on('error', reject);
    request.end('{}');
  });
  let proxy;
  let incoming;
  try {
    const upstreamUrl = await listen(upstream);
    proxy = await startReplayProxy({ mode: 'record', cassetteDir,
      upstreams: [{ logicalOrigin: 'http://fixture.test', transportOrigin: upstreamUrl }] });
    proxy.createRun('record', { resolveContext: (_context, _run, signal) => {
      signal.addEventListener('abort', () => aborted.resolve(), { once: true });
    } });
    incoming = await openStream(proxy, 'record');
    await appended.promise;
    incoming.response.destroy();
    await aborted.promise;
    releaseAppend.resolve();
    await proxy.waitFor('record', (state) => state.active === 0);
    assert.equal(proxy.completeRun('record').calls[0].writtenBytes, 0);
    assert.equal(Buffer.concat(incoming.chunks).length, 0);
    await proxy.seal();
    await proxy.close();
    await close(upstream);
    const cassette = await loadCassette(cassetteDir);
    const entry = [...cassette.entries.values()][0];
    assert.equal(entry.response.size, body.length, 'raw tape 保留代理确实观察到的字节');
    assert.equal(entry.response.submittedBytes, 0, '尚未提交 HTTP writer 的尾部不能算已发送');
    assert.equal(entry.response.protocolFacts.terminalMarker, 'seen', '协议观察与客户端收到的字节是不同事实');
    proxy = await startReplayProxy({ mode: 'replay', cassetteDir });
    proxy.createRun('replay');
    incoming = await openStream(proxy, 'replay');
    incoming.response.destroy();
    await proxy.waitFor('replay', (state) => state.active === 0);
    const summary = proxy.completeRun('replay');
    assert.equal(summary.calls[0].terminal, 'client_abort');
    assert.equal(summary.calls[0].writtenBytes, 0);
    assert.equal(Buffer.concat(incoming.chunks).length, 0);
    assert.equal(summary.upstreamCalls, 0);
  } finally {
    incoming?.response.destroy();
    releaseAppend.resolve();
    await proxy?.close();
    await close(upstream);
    prototype.writeFile = originalWrite;
    await rm(directory, { recursive: true, force: true });
  }
});

function receiveAbortedBody(proxy, run, onPrefix = () => {}) {
  return new Promise((resolve, reject) => {
    const request = http.request(`${proxy.url}/v1/chat/completions`, {
      method: 'POST', headers: contextHeaders({ run, ...CALL }, TARGET),
    }, (response) => {
      const chunks = [];
      response.on('data', (chunk) => { chunks.push(chunk); onPrefix(Buffer.concat(chunks)); });
      response.on('aborted', () => resolve({ body: Buffer.concat(chunks), status: response.statusCode }));
      response.on('error', () => {});
      response.on('end', () => reject(new Error('断流场景不能被伪装成完整 HTTP EOF')));
    });
    request.on('error', reject);
    request.setTimeout(2000, () => request.destroy(new Error('等待断流超时')));
    request.end('{}');
  });
}

test('UTF-8 与 SSE 残段原字节可离线重放，上游断流不会被记为用户取消', { timeout: 8000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-byte-abort-'));
  const cassetteDir = path.join(directory, 'cassette');
  const body = Buffer.from('data: {"choices":[{"delta":{"content":"你🚀"}}]}\n\ndata: {"unfinished":"');
  const canAbort = deferred();
  const upstream = http.createServer((request, response) => {
    request.resume();
    request.on('end', () => {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      // 刻意切开 UTF-8；断言字节序列，不断言 HTTP/TCP 分块被网络原样保留。
      for (let offset = 0; offset < body.length; offset += 7) response.write(body.subarray(offset, offset + 7));
      void canAbort.promise.then(() => response.destroy());
    });
  });
  let proxy;
  try {
    const upstreamUrl = await listen(upstream);
    proxy = await startReplayProxy({ mode: 'record', cassetteDir,
      upstreams: [{ logicalOrigin: 'http://fixture.test', transportOrigin: upstreamUrl }] });
    proxy.createRun('record');
    const recorded = await receiveAbortedBody(proxy, 'record', (prefix) => {
      if (prefix.length === body.length) canAbort.resolve();
    });
    assert.deepEqual(recorded.body, body);
    await proxy.waitFor('record', (state) => state.active === 0);
    assert.equal(proxy.completeRun('record').calls[0].terminal, 'upstream_abort');
    await proxy.seal();
    await proxy.close();
    await close(upstream);
    const cassette = await loadCassette(cassetteDir);
    const entry = [...cassette.entries.values()][0];
    assert.equal(entry.response.protocolFacts.terminalMarker, 'not_seen');
    const chunks = [];
    for await (const chunk of readResponseChunks(cassette, entry)) chunks.push(chunk.bytes);
    assert.deepEqual(Buffer.concat(chunks), body, '保留不完整 SSE 尾部，不补换行或 JSON');
    proxy = await startReplayProxy({ mode: 'replay', cassetteDir });
    proxy.createRun('replay');
    const replayed = await receiveAbortedBody(proxy, 'replay');
    assert.deepEqual(replayed, recorded);
    await proxy.waitFor('replay', (state) => state.active === 0);
    const summary = proxy.completeRun('replay');
    assert.equal(summary.calls[0].terminal, 'upstream_abort');
    assert.equal(summary.upstreamCalls, 0);
  } finally {
    canAbort.resolve();
    await proxy?.close();
    await close(upstream);
    await rm(directory, { recursive: true, force: true });
  }
});

async function delayReaderClose(directory, bodySize) {
  const probe = await open(path.join(directory, 'probe-reader'), 'w');
  const prototype = Object.getPrototypeOf(probe), originalRead = prototype.read;
  await probe.close();
  const entered = deferred(), release = deferred(), wrapped = new WeakSet();
  prototype.read = async function (...args) {
    const result = await originalRead.apply(this, args);
    const size = (await this.stat()).size;
    if (!wrapped.has(this) && result.bytesRead && size === bodySize) {
      wrapped.add(this);
      const originalClose = this.close.bind(this);
      this.close = async () => { entered.resolve(); await release.promise; return originalClose(); };
    }
    return result;
  };
  return {
    waitForClose: () => new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('reader.close 未进入控制门')), 1000);
      entered.promise.then(() => { clearTimeout(timer); resolve(); });
    }),
    release: release.resolve,
    restore() { prototype.read = originalRead; },
  };
}

test('客户端在完整 SSE 终止标记后关闭，慢文件清理不会把成功重放误记为取消', { timeout: 8000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-sse-terminal-'));
  const cassetteDir = path.join(directory, 'cassette');
  const body = 'data: {"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n';
  const upstream = http.createServer((request, response) => {
    request.resume();
    request.on('end', () => { response.writeHead(200, { 'content-type': 'text/event-stream' }); response.end(body); });
  });
  let proxy, slowClose;
  try {
    const upstreamUrl = await listen(upstream);
    proxy = await startReplayProxy({ mode: 'record', cassetteDir,
      upstreams: [{ logicalOrigin: 'http://fixture.test', transportOrigin: upstreamUrl }] });
    proxy.createRun('record');
    assert.equal((await send(proxy, 'record')).body, body);
    await proxy.waitFor('record', (state) => state.active === 0);
    proxy.completeRun('record');
    await proxy.seal();
    await proxy.close();
    await close(upstream);
    slowClose = await delayReaderClose(directory, Buffer.byteLength(body));
    proxy = await startReplayProxy({ mode: 'replay', cassetteDir });
    proxy.createRun('replay');
    const observed = await new Promise((resolve, reject) => {
      const request = http.request(`${proxy.url}/v1/chat/completions`, {
        method: 'POST', headers: { ...contextHeaders({ run: 'replay', ...CALL }, TARGET), 'content-type': 'application/json' },
      }, (response) => {
        let received = '';
        response.on('error', () => {});
        response.on('data', (chunk) => {
          received += chunk;
          if (received.includes('data: [DONE]\n\n')) { response.destroy(); resolve(received); }
        });
        response.on('end', () => { if (!received.includes('data: [DONE]\n\n')) reject(new Error(`缺少完整 SSE 终止标记：${received}`)); });
      });
      request.on('error', reject);
      request.setTimeout(2000, () => request.destroy(new Error('终止标记未到达')));
      request.end('{}');
    });
    assert.equal(observed, body);
    await slowClose.waitForClose();
    await proxy.waitFor('replay', (state) => state.calls[0]?.terminal === 'complete' && state.pendingCleanup === 1);
    assert.throws(() => proxy.completeRun('replay'), '网络完成不代表文件清理已经完成');
    slowClose.release();
    await proxy.waitFor('replay', (state) => state.active === 0);
    const summary = proxy.completeRun('replay');
    assert.equal(summary.calls[0].terminal, 'complete');
    assert.deepEqual(summary.errors, []);
    assert.equal(summary.upstreamCalls, 0);
  } finally {
    slowClose?.release();
    await proxy?.close();
    await close(upstream);
    slowClose?.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('重放响应已经结束后允许下一次真实重试，pending 文件清理仍阻止 run 完成', { timeout: 8000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-replay-close-retry-'));
  const cassetteDir = path.join(directory, 'cassette');
  const rateBody = '{"error":"rate-limit-before-file-close"}';
  let count = 0;
  const upstream = http.createServer((request, response) => {
    request.resume();
    request.on('end', () => {
      count++;
      response.writeHead(count === 1 ? 429 : 200, { 'content-type': 'application/json' });
      response.end(count === 1 ? rateBody : '{"recovered":true}');
    });
  });
  let proxy, slowClose;
  try {
    const upstreamUrl = await listen(upstream);
    proxy = await startReplayProxy({ mode: 'record', cassetteDir,
      upstreams: [{ logicalOrigin: 'http://fixture.test', transportOrigin: upstreamUrl }] });
    proxy.createRun('record');
    assert.equal((await send(proxy, 'record')).status, 429);
    assert.equal((await send(proxy, 'record', 2)).status, 200);
    await proxy.waitFor('record', (state) => state.active === 0);
    proxy.completeRun('record');
    await proxy.seal();
    await proxy.close();
    await close(upstream);
    slowClose = await delayReaderClose(directory, Buffer.byteLength(rateBody));
    proxy = await startReplayProxy({ mode: 'replay', cassetteDir });
    proxy.createRun('replay');
    assert.equal((await send(proxy, 'replay')).status, 429);
    await slowClose.waitForClose();
    assert.equal((await send(proxy, 'replay', 2)).status, 200, '前一次 reader.close 不应阻挡已收到完整错误响应的客户端重试');
    assert.throws(() => proxy.completeRun('replay'), '所有 response 都完成仍必须等待 reader 清理');
    slowClose.release();
    await proxy.waitFor('replay', (state) => state.active === 0);
    const summary = proxy.completeRun('replay');
    assert.equal(summary.calls.length, 2);
    assert.deepEqual(summary.errors, []);
    assert.equal(summary.upstreamCalls, 0);
  } finally {
    slowClose?.release();
    await proxy?.close();
    await close(upstream);
    slowClose?.restore();
    await rm(directory, { recursive: true, force: true });
  }
});
