import http from 'node:http';
import https from 'node:https';
import { performance } from 'node:perf_hooks';
import { setTimeout as delay } from 'node:timers/promises';
import { canonicalBody, decodeContext, identityOf, logicalTarget, ReplayError, semanticHeaders } from './matcher.mjs';
import { assertRequestMatches } from './matcher.mjs';
import { createCassetteWriter, loadCassette, readResponseChunks } from './cassette.mjs';
import { ReplayRun } from './run-state.mjs';
import { createProtocolObserver } from './protocol-facts.mjs';
import { derivePortableSse } from './bindings.mjs';
import { identityKey } from './matcher.mjs';

const CONTEXT_HEADER = 'x-peri-replay-context';
const TARGET_HEADER = 'x-peri-replay-target';
const HOP_HEADERS = new Set(['connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization', 'te', 'trailer', 'transfer-encoding', 'upgrade']);
const RESPONSE_HEADERS = ['content-type', 'content-encoding', 'content-length', 'retry-after', 'anthropic-ratelimit-requests-remaining', 'x-ratelimit-remaining-requests'];

export function contextHeaders(context, target) {
  return {
    [CONTEXT_HEADER]: Buffer.from(JSON.stringify({ v: 1, ...context })).toString('base64url'),
    [TARGET_HEADER]: target,
  };
}

function responseHeaders(headers) {
  return Object.fromEntries(RESPONSE_HEADERS.filter((name) => headers[name] !== undefined)
    .map((name) => [name, String(headers[name])]));
}

function upstreamHeaders(headers, target) {
  const connectionHeaders = String(headers.connection ?? '').toLowerCase().split(',').map((v) => v.trim());
  return Object.fromEntries(Object.entries(headers).filter(([name]) =>
    !HOP_HEADERS.has(name) && !connectionHeaders.includes(name) && !name.startsWith('x-peri-replay-') && name !== 'host')
    .concat([['host', target.host]]));
}

function mappedTarget(target, upstreams) {
  const logical = new URL(target);
  const mapping = upstreams.find((entry) => entry.logicalOrigin === logical.origin);
  if (!mapping) return logical;
  const transport = new URL(mapping.transportOrigin);
  transport.pathname = logical.pathname;
  transport.search = logical.search;
  return transport;
}

async function readRequest(request, limit) {
  const chunks = [];
  let length = 0;
  for await (const chunk of request) {
    length += chunk.length;
    if (length > limit) throw new ReplayError('request_too_large', '请求超过录制上限');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

function waitAbort(signal) {
  if (signal.aborted) return Promise.resolve();
  return new Promise((resolve) => signal.addEventListener('abort', resolve, { once: true }));
}

function raceAbort(work, signal) {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise((resolve, reject) => {
    const abort = () => { cleanup(); reject(signal.reason); };
    const cleanup = () => signal.removeEventListener('abort', abort);
    signal.addEventListener('abort', abort, { once: true });
    Promise.resolve(work).then((value) => { cleanup(); resolve(value); }, (error) => { cleanup(); reject(error); });
    if (signal.aborted) abort();
  });
}

async function writeBytes(response, bytes, signal, submitted) {
  if (signal.aborted) throw signal.reason;
  const writable = response.write(bytes);
  // write 返回 false 仅代表背压；这些字节已经进入 HTTP writer，不等待 drain 才计数。
  submitted?.();
  if (writable) return;
  await new Promise((resolve, reject) => {
    const drained = () => { cleanup(); resolve(); };
    const aborted = () => { cleanup(); reject(signal.reason); };
    const failed = (error) => { cleanup(); reject(error); };
    const cleanup = () => {
      response.off('drain', drained); response.off('error', failed);
      signal.removeEventListener('abort', aborted);
    };
    response.once('drain', drained); response.once('error', failed);
    signal.addEventListener('abort', aborted, { once: true });
    if (signal.aborted) aborted();
  });
}

async function finishResponse(response, signal) {
  if (response.writableFinished) return;
  if (signal.aborted) throw signal.reason;
  await new Promise((resolve, reject) => {
    const finished = () => { cleanup(); resolve(); };
    const aborted = () => { cleanup(); reject(signal.reason); };
    const failed = (error) => { cleanup(); reject(error); };
    const cleanup = () => {
      response.off('finish', finished); response.off('error', failed);
      signal.removeEventListener('abort', aborted);
    };
    response.once('finish', finished); response.once('error', failed);
    signal.addEventListener('abort', aborted, { once: true });
    if (!response.writableEnded) response.end();
    if (signal.aborted) aborted();
  });
}

async function forwardChunk(run, call, response, bytes, signal, tap, finishAtBytes) {
  let offset = 0;
  while (offset < bytes.length) {
    await run.waitAtGate(call, signal);
    if (signal.aborted) throw signal.reason;
    const length = run.permittedBytes(call, bytes.length - offset);
    const part = bytes.subarray(offset, offset + length);
    await tap?.(part);
    await writeBytes(response, part, signal, () => {
      run.progress(call, part.length);
      // fast 的完整回放已知 EOF。末字节和 HTTP end 同步提交，避免读文件 iterator 的
      // close/下一次 await 留出空档，让正常协议消费者先关闭连接而被误判为提前断流。
      // 恰好位于控制门边界时仍保留 runner 对 EOF 的控制。
      if (call.writtenBytes === finishAtBytes && run.permittedBytes(call, 1) > 0 && !response.writableEnded) response.end();
    });
    offset += length;
  }
}

/**
 * 本地独立录放服务。replay 分支不创建任何上游 request；run 必须由 runner 显式注册。
 * 代理记录的是 HTTP body 观察分块及发送事实，不把它当 TCP 包边界或应用消费确认。
 */
export async function startReplayProxy({
  mode, cassetteDir, upstreams = [], host = '127.0.0.1', port = 0,
  timing = 'fast', maxRequestBytes = 16 * 1024 * 1024,
  maxResponseBytes = 64 * 1024 * 1024, requestTimeoutMs = 30000, provenance = {}, world, worldDir,
} = {}) {
  if (!['record', 'replay'].includes(mode)) throw new ReplayError('invalid_mode', 'mode 只能为 record 或 replay');
  if (!cassetteDir) throw new ReplayError('invalid_directory', '必须指定 cassetteDir');
  if (!['127.0.0.1', '::1', 'localhost'].includes(host)) throw new ReplayError('invalid_host', '录放服务只监听 loopback');
  if (!['fast', 'recorded'].includes(timing)) throw new ReplayError('invalid_timing', 'timing 只能为 fast 或 recorded');
  if (mode === 'replay' && upstreams.length) throw new ReplayError('offline_configuration', 'replay 不接受上游配置');
  for (const mapping of upstreams) {
    const logical = new URL(logicalTarget(mapping.logicalOrigin));
    const transport = new URL(logicalTarget(mapping.transportOrigin));
    if (logical.origin !== mapping.logicalOrigin || transport.origin !== mapping.transportOrigin) throw new ReplayError('invalid_mapping', '上游映射仅允许 origin');
  }
  const writer = mode === 'record' ? await createCassetteWriter(cassetteDir, { provenance, maxResponseBytes, world }) : null;
  const cassette = mode === 'replay' ? await loadCassette(cassetteDir, { worldDir }) : null;
  const runs = new Map();
  const globalErrors = [];
  const active = new Set();
  const controllers = new Set();
  let closing = false;

  const server = http.createServer((request, response) => {
    const work = handle(request, response);
    active.add(work);
    work.finally(() => active.delete(work));
  });
  server.on('clientError', (error, socket) => {
    globalErrors.push({ code: 'invalid_http', message: error.message });
    socket.destroy();
  });

  async function handle(request, response) {
    let run, call, capture, protocolObserver;
    let capturePending = false;
    let replayCleanupPending = false;
    let finishListener;
    const startedAt = performance.now();
    const elapsed = () => performance.now() - startedAt;
    const controller = new AbortController();
    controllers.add(controller);
    let clientAborted = false;
    let proxyTerminating = false;
    const closed = () => {
      if (!response.writableFinished && !proxyTerminating) {
        clientAborted = true;
        controller.abort(new ReplayError('client_abort', '客户端关闭了响应连接'));
      }
    };
    response.on('close', closed);
    response.on('error', () => {});
    const timeout = setTimeout(() => controller.abort(new ReplayError('request_timeout', '录放请求超过等待期限')), requestTimeoutMs);
    try {
      if (closing) throw new ReplayError('proxy_closing', '录放服务正在关闭');
      const rawContext = decodeContext(request.headers[CONTEXT_HEADER]);
      run = runs.get(rawContext.run);
      if (!run) throw new ReplayError('unknown_run', 'run 尚未由 runner 注册', { run: rawContext.run });
      const context = await raceAbort(run.resolveContext(rawContext, controller.signal), controller.signal);
      const target = logicalTarget(request.headers[TARGET_HEADER]);
      const url = new URL(target);
      if (`${url.pathname}${url.search}` !== request.url) throw new ReplayError('target_path_mismatch', '代理路由改变了原始 path 或 query');
      const body = await raceAbort(readRequest(request, maxRequestBytes), controller.signal);
      call = run.reserve(context);
      const input = { method: request.method, target, headers: semanticHeaders(request.headers), body };
      if (run.options.onRequest) {
        await raceAbort(run.options.onRequest({
          context: structuredClone(context),
          request: { ...input, headers: { ...input.headers }, body: Buffer.from(input.body) },
          expected: call.entry ? structuredClone(call.entry.request) : undefined,
        }), controller.signal);
      }
      const rules = mode === 'replay' ? call.entry.requestRules : (typeof run.options.requestRules === 'function' ? run.options.requestRules(context) : run.options.requestRules ?? []);
      const normalization = {
        bindings: run.options.bindings,
        owner: run.options.bindingOwnerFor?.(context) ?? context.owner,
        rules,
      };
      if (mode === 'record') {
        input.canonicalBody = canonicalBody(body, normalization);
        capture = await writer.begin(identityOf(context), input, rules, { bindings: normalization.bindings, owner: normalization.owner });
        capturePending = true;
        run.pendingDurable++;
        await record(input, request.headers, response, run, call, capture, controller.signal, elapsed, () => clientAborted);
      } else {
        assertRequestMatches(call.entry.request, input, normalization);
        await replay(response, run, call, controller.signal, elapsed, () => clientAborted);
      }
    } catch (error) {
      if (error.code === 'ABORT_ERR' && controller.signal.aborted) error = controller.signal.reason;
      if (error.code === 'client_abort' && call) {
        if (capture && protocolObserver) capture.setProtocolFacts(protocolObserver.snapshot());
        if (capture) capture.setSubmittedBytes(call.writtenBytes);
        run.finish(call, 'client_abort');
        if (capture) await capture.finish('client_abort', elapsed()).catch((failure) => run.fail(failure));
      } else {
        if (run) run.fail(error); else globalErrors.push({ code: error.code ?? 'proxy_error', message: error.message, details: error.details ?? {} });
        if (capture) await capture.invalidate(error).catch((failure) => run?.fail(failure));
        if (call) { call.state = 'abort'; call.terminal = 'proxy_error'; run.emit('change'); }
        proxyTerminating = true;
        if (!response.headersSent && !response.destroyed) {
          response.writeHead(409, { 'content-type': 'application/json' });
          response.end(JSON.stringify({ error: error.code ?? 'proxy_error', message: error.message, details: error.details ?? {} }));
        } else response.destroy();
      }
    } finally {
      if (capturePending) { run.pendingDurable--; run.emit('change'); }
      if (replayCleanupPending) { run.pendingCleanup--; run.emit('change'); }
      if (finishListener) response.off('finish', finishListener);
      clearTimeout(timeout);
      response.off('close', closed);
      controllers.delete(controller);
      if (controller.signal.aborted && !request.complete) request.destroy();
    }

    async function record(input, originalHeaders, outgoing, state, transaction, tap, signal, clock, wasClientAbort) {
      const target = mappedTarget(input.target, upstreams);
      state.upstreamCalls++;
      let upstream;
      try {
        upstream = await new Promise((resolve, reject) => {
          const client = (target.protocol === 'https:' ? https : http).request(target, {
            method: input.method, headers: upstreamHeaders(originalHeaders, target), signal,
          }, resolve);
          client.on('error', reject);
          client.end(input.body);
        });
      } catch (error) {
        if (signal.aborted) throw signal.reason;
        // 没有收到上游 HTTP 响应时保留连接失败，不能伪造一个 HTTP 200/502 响应。
        tap.setProtocolFacts({ protocol: 'unknown', terminalMarker: 'unrecognized', reason: 'no_response_headers' });
        state.finish(transaction, 'upstream_error');
        await tap.finish('upstream_error', clock(), error.code ?? 'connection_error');
        proxyTerminating = true; outgoing.destroy();
        return;
      }
      const headers = responseHeaders(upstream.headers);
      protocolObserver = createProtocolObserver(input.target, headers);
      tap.setHeaders(upstream.statusCode, headers, clock());
      outgoing.writeHead(upstream.statusCode, headers);
      outgoing.flushHeaders();
      state.start(transaction);
      try {
        for await (const bytes of upstream) {
          await forwardChunk(state, transaction, outgoing, bytes, signal, async (part) => {
            protocolObserver.feed(part);
            await tap.append(part, clock());
          });
        }
        await state.waitAtGate(transaction, signal);
        await finishResponse(outgoing, signal);
        tap.setProtocolFacts(protocolObserver.snapshot());
        tap.setSubmittedBytes(transaction.writtenBytes);
        state.finish(transaction, 'complete');
        await tap.finish('complete', clock());
      } catch (error) {
        upstream.destroy();
        if (signal.aborted) throw signal.reason;
        if (error instanceof ReplayError) throw error;
        const kind = wasClientAbort() ? 'client_abort' : 'upstream_abort';
        tap.setProtocolFacts(protocolObserver.snapshot());
        tap.setSubmittedBytes(transaction.writtenBytes);
        state.finish(transaction, kind);
        await tap.finish(kind, clock(), error.code ?? 'body_error');
        proxyTerminating = true; outgoing.destroy();
      }
    }

    async function replay(outgoing, state, transaction, signal, clock, wasClientAbort) {
      const entry = transaction.entry;
      const response = entry.response;
      const pacing = state.options.timing ?? timing;
      const portable = state.options.portableResponses?.find((plan) => identityKey(plan.identity) === identityKey(transaction.identity));
      let portableBody;
      let headers = response.headers;
      if (portable) {
        if (pacing !== 'fast' || response.terminal.kind !== 'complete' || response.headers['content-encoding']) {
          throw new ReplayError('unsupported_portable_response', 'portable 仅支持完整、未压缩、fast 模式响应');
        }
        const raw = [];
        for await (const chunk of readResponseChunks(cassette, entry)) raw.push(chunk.bytes);
        const derived = derivePortableSse({
          ...portable, raw: Buffer.concat(raw), registry: state.options.bindings,
          owner: portable.owner ?? state.options.bindingOwnerFor?.(transaction.identity) ?? transaction.identity.owner,
        });
        if (portable.sourceSha256 !== undefined && portable.sourceSha256 !== derived.provenance.sourceSha256) {
          throw new ReplayError('portable_source_mismatch', 'portable 规则不属于当前原始响应');
        }
        portableBody = derived.body;
        transaction.derivedFixture = derived.provenance;
        transaction.expectedResponseBytes = portableBody.length;
        headers = { ...response.headers };
        if (headers['content-length'] !== undefined) headers['content-length'] = String(portableBody.length);
      }
      const until = async (atMs) => {
        if (pacing === 'recorded' && atMs > clock()) await delay(atMs - clock(), undefined, { signal });
      };
      if (response.terminal.kind === 'complete') {
        finishListener = () => {
          if (replayCleanupPending || transaction.writtenBytes !== (portableBody?.length ?? response.submittedBytes)) return;
          // HTTP 已完成便允许该 logical call 的真实 retry；剩余文件清理仍阻止 run complete。
          replayCleanupPending = true;
          state.pendingCleanup++;
          state.finish(transaction, 'complete');
        };
        outgoing.once('finish', finishListener);
      }
      if (response.status !== null) {
        await until(response.headersAtMs ?? 0);
        if (signal.aborted) throw signal.reason;
        outgoing.writeHead(response.status, headers);
        outgoing.flushHeaders();
      }
      state.start(transaction);
      const finishAtBytes = pacing === 'fast' && response.terminal.kind === 'complete'
        ? portableBody?.length ?? response.submittedBytes : undefined;
      if (finishAtBytes === 0 && state.permittedBytes(transaction, 1) > 0) outgoing.end();
      if (portableBody) {
        for (let offset = 0; offset < portableBody.length; offset += 16384) {
          await forwardChunk(state, transaction, outgoing, portableBody.subarray(offset, offset + 16384), signal, undefined, finishAtBytes);
        }
      } else {
        const cut = response.submittedBytes;
        for await (const chunk of readResponseChunks(cassette, entry)) {
          if (transaction.writtenBytes >= cut) break;
          await until(chunk.atMs);
          const bytes = chunk.bytes.subarray(0, Math.min(chunk.bytes.length, cut - transaction.writtenBytes));
          await forwardChunk(state, transaction, outgoing, bytes, signal, undefined, finishAtBytes);
        }
      }
      await state.waitAtGate(transaction, signal);
      await until(response.terminal.atMs);
      switch (response.terminal.kind) {
        case 'complete':
          await finishResponse(outgoing, signal);
          if (!replayCleanupPending) state.finish(transaction, 'complete');
          return;
        case 'client_abort':
          // 取消必须由真实客户端触发；代理不能替它完成产品行为。
          await waitAbort(signal);
          throw signal.reason;
        case 'upstream_abort':
        case 'upstream_error':
          if (signal.aborted) throw signal.reason;
          state.finish(transaction, wasClientAbort() ? 'client_abort' : response.terminal.kind);
          proxyTerminating = true; outgoing.destroy();
          return;
      }
    }
  }

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, host, () => { server.off('error', reject); resolve(); });
  });
  const address = server.address();
  const url = `http://${host.includes(':') ? `[${host}]` : host}:${address.port}`;
  const getRun = (id) => {
    const run = runs.get(id);
    if (!run) throw new ReplayError('unknown_run', 'run 不存在');
    return run;
  };
  return {
    url,
    mode,
    createRun(id, options = {}) {
      if (runs.has(id)) throw new ReplayError('duplicate_run', '同一个 run ID 不得重置或重复注册');
      if (mode === 'record' && runs.size) throw new ReplayError('record_run_exists', '一次录制只能有一个 run，多会话在该 run 内并发');
      if (mode === 'record' && options.portableResponses?.length) throw new ReplayError('raw_recording', '录制仅保存 raw；portable 只能显式派生用于重放');
      if (options.bindings && options.bindings.runId !== id) throw new ReplayError('binding_run_mismatch', '绑定表只能用于其所属 run');
      if (options.timing !== undefined && !['fast', 'recorded'].includes(options.timing)) throw new ReplayError('invalid_timing', 'run timing 无效');
      const run = new ReplayRun(id, mode, cassette?.entries ?? writer.entries, options);
      runs.set(id, run);
      return run;
    },
    snapshot(id) { return getRun(id).snapshot(); },
    waitFor(id, predicate, options) { return getRun(id).waitFor(predicate, options); },
    release(id, identity, throughBytes) { return getRun(id).release(identity, throughBytes); },
    completeRun(id) {
      if (globalErrors.length) throw new ReplayError('unattributed_requests', '存在无法归属的请求', { errors: globalErrors });
      return getRun(id).complete();
    },
    async seal() {
      if (!writer) throw new ReplayError('read_only_cassette', 'replay 不能修改 cassette');
      if (globalErrors.length || !runs.size || [...runs.values()].some((run) => !run.finalized || run.errors.length)) {
        throw new ReplayError('unsealable_run', '所有录制 run 必须先成功 complete');
      }
      return writer.seal();
    },
    async close() {
      closing = true;
      for (const controller of controllers) controller.abort(new ReplayError('proxy_shutdown', 'runner 关闭了录放服务，不是产品主动取消'));
      const stopped = new Promise((resolve) => server.close(resolve));
      server.closeAllConnections();
      await Promise.allSettled([...active]);
      await stopped;
    },
  };
}
