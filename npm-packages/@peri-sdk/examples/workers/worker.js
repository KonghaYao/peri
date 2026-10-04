import Module from './peri-wasm.js';
import wasmModule from './peri_wasm.wasm';

let loaded;

function withTimeout(promise, operation) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${operation} timed out`)), 20_000);
    }),
  ]).finally(() => clearTimeout(timer));
}

async function requestAcp(acp, id, method, params) {
  await withTimeout(acp.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })), `send ${method}`);
  const notifications = [];
  for (;;) {
    const raw = await withTimeout(acp.recv(), `recv ${method}`);
    if (raw == null) throw new Error(`ACP port closed during ${method}`);
    const frame = JSON.parse(raw);
    if (frame.id === id) return { frame, notifications };
    if (frame.id != null) throw new Error(`Unexpected ACP response during ${method}`);
    notifications.push(frame.method);
  }
}

export default {
  async fetch(request, env) {
    let acp;
    try {
      loaded ??= Module({
        mainScriptUrlOrBlob: 'file:///bundle/peri-wasm.js',
        instantiateWasm(imports, receiveInstance) {
          WebAssembly.instantiate(wasmModule, imports).then(receiveInstance);
        },
      });
      const wasm = await withTimeout(loaded, 'load peri-wasm');
      const config = {
        cwd: '/workspace',
        settings: { config: {
          active_alias: 'sonnet',
          providers: [{
            id: 'fixture', type: 'openai', apiKey: 'fixture-key', baseUrl: env.MODEL_URL,
          }],
          profiles: { sonnet: { provider: 'fixture', model: 'fixture-model' } },
        } },
        storage: { url: env.STORAGE_URL, authToken: 'local-dev' },
        machineId: '00000000-0000-4000-8000-000000000004',
      };
      acp = await withTimeout(wasm.PeriWasmAcp.start(JSON.stringify(config)), 'start ACP');
      const initialized = (await requestAcp(acp, 1, 'initialize', {
        protocolVersion: 1, clientCapabilities: {},
      })).frame;
      if (initialized.result?.protocolVersion !== 1) throw new Error('ACP initialize failed');

      const restoreId = new URL(request.url).searchParams.get('sessionId');
      if (restoreId) {
        const listed = (await requestAcp(acp, 2, 'session/list', { cwd: '/workspace' })).frame;
        const loadedSession = (await requestAcp(acp, 3, 'session/load', {
          sessionId: restoreId, cwd: '/workspace', mcpServers: [],
        })).frame;
        return Response.json({
          listed: listed.result?.sessions?.some((session) => session.sessionId === restoreId),
          loaded: !!loadedSession.result?.modes,
          error: listed.error ?? loadedSession.error,
        });
      }

      const created = (await requestAcp(acp, 2, 'session/new', {
        cwd: '/workspace', mcpServers: [],
      })).frame;
      const sessionId = created.result?.sessionId;
      if (!sessionId) throw new Error(`session/new failed: ${JSON.stringify(created.error)}`);
      const prompt = await requestAcp(acp, 3, 'session/prompt', {
        sessionId, prompt: [{ type: 'text', text: 'Reply once' }],
      });
      return Response.json({
        sessionId,
        stopReason: prompt.frame.result?.stopReason,
        notifications: prompt.notifications.filter((name) => name === 'session/update').length,
        error: prompt.frame.error,
      });
    } catch (error) {
      return Response.json({ error: String(error) }, { status: 500 });
    } finally {
      if (acp) {
        const closed = await withTimeout(acp.close(), 'close ACP').then(() => true, () => false);
        if (closed) acp.free();
      }
    }
  },
};
