/**
 * 本机 Wrangler 探针：在 workerd 上直接驱动打包的 peri-wasm ACP Host，不使用 SDK 进程能力。
 *
 * 只用当前 Host 方法表（`peri-acp/src/host/server_loop.rs` 的请求分派）：
 *  - 会话：`session/new` / `session/list` / `session/load` / `session/close`
 *  - Turn：默认走 `session/input/{snapshot,enqueue}` 队列路径，与 SDK `Session.send` 相同；
 *    `?turn=prompt` 走 `session/prompt`，这是 SDK 之外的直连 turn 路径（peri-cf 也在用），不是遗留方法。
 *  - 取消：`session/cancel` 是通知，不是请求。
 *  - `?host=sdk`：peri-cf 形态——调用 SDK 的 `startPeriWasmHost` 且**只注入 `moduleFactory`**，
 *    不提供可解析的 `moduleUrl`。workerd 里 `import.meta.url` 是 undefined，SDK 不得在注入
 *    factory 时再去解析产物 URL。
 *
 * 队列路径的执行由 Peri mailbox 调度：入队回执只说明已受理，turn 结束必须等
 * `peri/agent_event_done`，投递必须等 `peri/agent_event` 的 `user_input_delivered`。
 * Store 位置与凭证全部显式传入（`STORAGE_URL` / `MODEL_URL`），不读宿主进程环境。
 */
import Module from './peri-wasm.js';
import wasmModule from './peri_wasm.wasm';
import { createNodeSchedulerPort, startPeriWasmHost } from './wasm-host.js';

const CWD = '/workspace';
const TURN_BUDGET_MS = 60_000;
const CLOSE_BUDGET_MS = 10_000;
const CLOSE_RETRY_MS = 250;
/** Peri 用 -32010 表达“关闭尚未结算，可重试”。 */
const INCOMPLETE_CLOSE = -32010;

/** 能力按实际消费声明；队列与事件通道都必须显式协商，否则 Host 不发送对应通知。 */
const CAPABILITIES = {
  'peri.userInputQueue': true,
  'peri.agentEvent': true,
  'peri.agentEventDone': true,
  'peri.unstableEvent': true,
  'peri.replay': true,
};

let modulePromise;

const sleep = (milliseconds) => new Promise((done) => setTimeout(done, milliseconds));

function withTimeout(promise, operation, milliseconds = 20_000) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${operation} timed out`)), milliseconds);
    }),
  ]).finally(() => clearTimeout(timer));
}

/** 模块工厂失败不缓存，否则一次加载失败会让后续请求永远复用被拒绝的 promise。 */
async function loadModule(env) {
  if (!modulePromise) {
    modulePromise = Module({
      mainScriptUrlOrBlob: 'file:///bundle/peri-wasm.js',
      instantiateWasm(imports, receiveInstance) {
        WebAssembly.instantiate(wasmModule, imports).then(receiveInstance);
      },
    }).catch((error) => {
      modulePromise = undefined;
      throw error;
    });
  }
  return withTimeout(modulePromise, 'load peri-wasm');
}

function hostConfig(env) {
  return {
    cwd: CWD,
    settings: { config: {
      active_alias: 'sonnet',
      providers: [{ id: 'fixture', type: 'openai', apiKey: 'fixture-key', baseUrl: env.MODEL_URL }],
      profiles: { sonnet: { provider: 'fixture', model: 'fixture-model' } },
    } },
    storage: { url: env.STORAGE_URL, authToken: 'local-dev' },
    machineId: '00000000-0000-4000-8000-000000000004',
  };
}

async function notify(acp, method, params) {
  await withTimeout(acp.send(JSON.stringify({ jsonrpc: '2.0', method, params })), `notify ${method}`);
}

/** 一个 ACP 请求；期间到达的通知交给 observe。 */
async function request(acp, id, method, params, observe) {
  await withTimeout(acp.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })), `send ${method}`);
  for (;;) {
    const raw = await withTimeout(acp.recv(), `recv ${method}`);
    if (raw == null) throw new Error(`ACP port closed during ${method}`);
    const frame = JSON.parse(raw);
    if (frame.id === id) return frame;
    if (frame.id != null) throw new Error(`Unexpected ACP response during ${method}`);
    observe(frame);
  }
}

/** 队列路径的 turn 结束信号是传输层通知，不是任何请求的响应。 */
async function waitForTurnEnd(acp, sessionId, observe) {
  const deadline = Date.now() + TURN_BUDGET_MS;
  for (;;) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error('turn did not finish in time');
    const raw = await withTimeout(acp.recv(), 'await turn end', remaining);
    if (raw == null) throw new Error('ACP port closed while awaiting turn end');
    const frame = JSON.parse(raw);
    if (frame.id != null) throw new Error('Unexpected ACP response while awaiting turn end');
    observe(frame);
    if (frame.method === 'peri/agent_event_done' && frame.params?.sessionId === sessionId)
      return { stopReason: frame.params?.stopReason };
  }
}

/** 关闭是有界重放：Host 未结算时重试，直到预算耗尽。 */
async function closeSession(acp, sessionId, observe) {
  const deadline = Date.now() + CLOSE_BUDGET_MS;
  let attempts = 0;
  for (;;) {
    attempts++;
    const frame = await request(acp, 900 + attempts, 'session/close', { sessionId }, observe);
    if (!frame.error) return { closed: true, attempts };
    if (frame.error.code !== INCOMPLETE_CLOSE || Date.now() >= deadline)
      return { closed: false, attempts, error: frame.error };
    await sleep(CLOSE_RETRY_MS);
  }
}

/** peri-cf 形态：宿主只注入 moduleFactory，没有可解析的产物 URL。 */
async function probeSdkHost(env) {
  const transport = await startPeriWasmHost({
    configJson: JSON.stringify(hostConfig(env)),
    moduleFactory: (options) => Module({
      mainScriptUrlOrBlob: 'file:///bundle/peri-wasm.js',
      instantiateWasm(imports, receiveInstance) {
        WebAssembly.instantiate(wasmModule, imports).then(receiveInstance);
      },
      ...options,
    }),
    ports: { scheduler: createNodeSchedulerPort() },
  });
  try {
    // transport.request<T> 的默认返回类型是 unknown，这里显式标注用到的响应字段。
    const initialized = /** @type {{ protocolVersion?: number }} */ (await transport.request('initialize', {
      protocolVersion: 1, clientCapabilities: { _meta: CAPABILITIES },
    }));
    const created = /** @type {{ sessionId?: string }} */ (await transport.request('session/new', { cwd: CWD, mcpServers: [] }));
    const sessionId = created?.sessionId;
    if (!sessionId) throw new Error(`session/new failed: ${JSON.stringify(created)}`);
    await transport.request('session/close', { sessionId });
    return Response.json({
      host: 'sdk',
      importMetaUrl: String(import.meta.url),
      protocolVersion: initialized?.protocolVersion,
      sessionId,
      closed: true,
    });
  } finally {
    await transport.close();
  }
}

export default {
  async fetch(request_, env) {
    const url = new URL(request_.url);
    const turn = url.searchParams.get('turn') === 'prompt' ? 'prompt' : 'queue';
    const notifications = [];
    const events = [];
    const observe = (frame) => {
      notifications.push(frame.method);
      if (frame.method === 'peri/agent_event') {
        try { events.push(JSON.parse(frame.params.event_json)); } catch { /* 诊断通道不阻断请求 */ }
      }
    };
    let acp;
    let sessionId;
    try {
      // peri-cf 形态放在同一错误通道里，失败原因随响应体返回而不是 workerd 错误页。
      if (url.searchParams.get('host') === 'sdk') return await probeSdkHost(env);
      const wasm = await loadModule(env);
      acp = await withTimeout(wasm.PeriWasmAcp.start(JSON.stringify(hostConfig(env))), 'start ACP');
      const initialized = await request(acp, 1, 'initialize', {
        protocolVersion: 1, clientCapabilities: { _meta: CAPABILITIES },
      }, observe);
      if (initialized.result?.protocolVersion !== 1) throw new Error('ACP initialize failed');

      const restoreId = url.searchParams.get('sessionId');
      if (restoreId) {
        const listed = await request(acp, 2, 'session/list', { cwd: CWD }, observe);
        const loaded = await request(acp, 3, 'session/load', {
          sessionId: restoreId, cwd: CWD, mcpServers: [],
        }, observe);
        sessionId = restoreId;
        const closed = await closeSession(acp, sessionId, observe);
        return Response.json({
          restored: true,
          listed: !!listed.result?.sessions?.some((session) => session.sessionId === restoreId),
          loaded: !!loaded.result?.modes,
          notifications: notifications.length,
          closed: closed.closed,
          closeAttempts: closed.attempts,
          error: listed.error ?? loaded.error ?? closed.error,
        });
      }

      const created = await request(acp, 2, 'session/new', { cwd: CWD, mcpServers: [] }, observe);
      sessionId = created.result?.sessionId;
      if (!sessionId) throw new Error(`session/new failed: ${JSON.stringify(created.error)}`);

      let outcome;
      if (turn === 'prompt') {
        const prompted = await request(acp, 3, 'session/prompt', {
          sessionId, prompt: [{ type: 'text', text: 'Reply once' }],
        }, observe);
        outcome = { turn, stopReason: prompted.result?.stopReason, error: prompted.error };
      } else {
        const snapshot = await request(acp, 3, 'session/input/snapshot', { sessionId }, observe);
        const generation = snapshot.result?.generation;
        if (!generation) throw new Error(`session/input/snapshot failed: ${JSON.stringify(snapshot.error)}`);
        const inputId = crypto.randomUUID();
        const enqueued = await request(acp, 4, 'session/input/enqueue', {
          sessionId, generation, commandId: `${inputId}:enqueue`, inputId,
          content: 'Reply once', originalDraft: 'Reply once',
        }, observe);
        const state = enqueued.result?.results?.find((item) => item.inputId === inputId)?.state;
        const ended = await waitForTurnEnd(acp, sessionId, observe);
        outcome = {
          turn,
          state,
          stopReason: ended.stopReason,
          delivered: events.some((event) => event.type === 'user_input_delivered' && event.value?.input_id === inputId),
          error: enqueued.error,
        };
      }
      const closed = await closeSession(acp, sessionId, observe);
      return Response.json({
        sessionId,
        ...outcome,
        events: events.map((event) => event.type),
        notifications: notifications.length,
        closed: closed.closed,
        closeAttempts: closed.attempts,
        error: outcome.error ?? closed.error,
      });
    } catch (error) {
      // 未完成的 turn 必须先取消，否则 CloseCoordinator 无法结算。
      let cleanupError;
      if (acp && sessionId) {
        try { await notify(acp, 'session/cancel', { sessionId }); }
        catch (failure) { cleanupError = String(failure); }
        try { await closeSession(acp, sessionId, observe); }
        catch (failure) { cleanupError = String(failure); }
      }
      return Response.json({ error: String(error), ...(cleanupError ? { cleanupError } : {}) }, { status: 500 });
    } finally {
      if (acp) {
        const closed = await withTimeout(acp.close(), 'close ACP').then(() => true, () => false);
        if (closed) acp.free();
      }
    }
  },
};
