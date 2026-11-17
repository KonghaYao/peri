import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { createHostResourceOwner } from '@peri-code/rivet/rivet';
import type { ChatRuntimeStatus } from '@shared/chat';
import {
  createChatEngine,
  createChatEngineWithStoreProbe,
  NotConfiguredChatEngine,
  probeChatRuntime,
  RivetChatEngine,
} from '../../../server/chats/engine';
import { StoreProbeChatEngine } from '../../../server/chats/store-recovery';
import { AppError } from '../../../server/errors';
import type { StartedRegistry } from '../../../server/rivet/registry';
import { testEnv } from '../helpers';

const READY_ENV = {
  storeToken: 'store-token',
  rivetStoragePath: '/tmp/agent-builder/.data/rivet',
};

describe('probeChatRuntime', () => {
  test('WASM host only reads tracing environment values, never Provider key getters', () => {
    const source = readFileSync(new URL('../../../server/rivet/host.ts', import.meta.url), 'utf8');
    const start = source.indexOf('function wasmEnvFromProcess');
    const body = source.slice(start, source.indexOf('\n/**', start));
    const js = new Bun.Transpiler({ loader: 'ts', target: 'bun' }).transformSync(body);
    let reads = 0;
    const reject = () => {
      reads++;
      throw new Error('unexpected Provider read');
    };
    const env = Object.defineProperties(
      { LANGFUSE_ENABLED: 'false' },
      {
        ANTHROPIC_API_KEY: { enumerable: true, get: reject },
        OPENAI_API_KEY: { enumerable: true, get: reject },
      },
    );
    const collect = new Function('process', `${js}\nreturn wasmEnvFromProcess();`);
    expect(collect({ env })).toEqual({ LANGFUSE_ENABLED: 'false' });
    expect(reads).toBe(0);
  });

  test('reports every missing configuration input without exposing values', () => {
    const status = probeChatRuntime(testEnv());
    expect(status.mode).toBe('not_configured');
    expect(status.reasons).toEqual(['store_token_missing', 'rivet_storage_path_missing']);
    expect(JSON.stringify(status)).not.toContain('sk-');
  });

  test('enables infrastructure with Store, wasm and storage path independently of Provider credentials', () => {
    const status = probeChatRuntime({ ...testEnv(), ...READY_ENV });
    expect(status).toEqual({ mode: 'wasm', reasons: [] });
  });

  test('does not enable infrastructure without a Store token even with a legacy env sentinel', () => {
    const env = Object.assign(testEnv(), {
      anthropicApiKey: crypto.randomUUID(),
      rivetStoragePath: '/tmp/agent-builder/.data/rivet',
    });
    const status = probeChatRuntime(env);
    expect(status.mode).toBe('not_configured');
    expect(status.reasons).toContain('store_token_missing');
  });
});

describe('NotConfiguredChatEngine', () => {
  test('reports the missing configuration reasons in health', () => {
    const engine = new NotConfiguredChatEngine({
      mode: 'not_configured',
      reasons: ['store_token_missing', 'provider_key_missing'],
    });

    expect(engine.health()).toEqual({
      mode: 'not_configured',
      status: 'unknown',
      reasons: ['store_token_missing', 'provider_key_missing'],
    });
  });

  test('refuses to start and names the reasons in the error details', async () => {
    const engine = createChatEngine(testEnv());
    expect(engine).toBeInstanceOf(NotConfiguredChatEngine);
    expect(engine.health()).toEqual({
      mode: 'not_configured',
      status: 'unknown',
      reasons: ['store_token_missing', 'rivet_storage_path_missing'],
    });

    await expect(engine.start()).rejects.toBeInstanceOf(AppError);
    await expect(engine.start()).rejects.toMatchObject({
      code: 'engine_unavailable',
      details: { reasons: ['store_token_missing', 'rivet_storage_path_missing'] },
    });
  });
});

describe('createChatEngineWithStoreProbe', () => {
  const readyEnv = () => ({ ...testEnv(), ...READY_ENV });

  test('reports store_unreachable instead of claiming the Store works', async () => {
    const engine = await createChatEngineWithStoreProbe(readyEnv(), {
      probe: async () => {
        throw new Error('ECONNREFUSED');
      },
    });

    expect(engine).toBeInstanceOf(StoreProbeChatEngine);
    expect(engine.status).toEqual({ mode: 'not_configured', reasons: ['store_unreachable'] });
    await expect(engine.start()).rejects.toMatchObject({
      code: 'engine_unavailable',
      details: { reasons: ['store_unreachable'] },
      retryable: true,
    });
  });

  test('keeps the real runtime when the Store answers a read', async () => {
    const engine = await createChatEngineWithStoreProbe(readyEnv(), { probe: async () => {} });

    // Even a healthy boot is wrapped, so a later Store loss degrades instead of serving blindly.
    expect(engine).toBeInstanceOf(StoreProbeChatEngine);
    expect(engine.status).toEqual({ mode: 'wasm', reasons: [] });
    expect(engine.health()).toEqual({ mode: 'embedded', status: 'unknown' });
  });

  test('does not probe the Store when configuration is incomplete', async () => {
    let probed = false;
    const engine = await createChatEngineWithStoreProbe(testEnv(), {
      probe: async () => {
        probed = true;
      },
    });

    expect(probed).toBe(false);
    expect(engine).toBeInstanceOf(NotConfiguredChatEngine);
    expect(engine.status.reasons).toContain('store_token_missing');
  });
});

describe('RivetChatEngine shutdown', () => {
  const status: ChatRuntimeStatus = { mode: 'wasm', reasons: [] };

  test('waits for an in-flight start and stops the registry it created', async () => {
    const events: string[] = [];
    const deferred: { release: ((started: StartedRegistry) => void) | null } = { release: null };
    const engine = new RivetChatEngine(
      { ...testEnv(), ...READY_ENV },
      status,
      '/tmp/peri-wasm.js',
      createHostResourceOwner(),
      null,
      {
        startRegistry: () =>
          new Promise<StartedRegistry>((resolve) => {
            deferred.release = resolve;
          }),
      },
    );

    const start = engine.start();
    const shutdown = engine.shutdown();
    if (deferred.release === null) throw new Error('startRegistry was not called');
    deferred.release({
      registry: {} as StartedRegistry['registry'],
      client: {} as StartedRegistry['client'],
      endpoint: 'http://127.0.0.1:8961',
      shutdown: async () => {
        events.push('registry-shutdown');
      },
    });

    await start;
    await shutdown;
    expect(events).toEqual(['registry-shutdown']);
    expect(engine.health()).toEqual({ mode: 'embedded', status: 'unknown' });
    expect(() => engine.actor('chat-1')).toThrow(AppError);
  });

  test('retains registry responsibility when shutdown rejects, including the Store wrapper', async () => {
    let calls = 0;
    const failure = new Error('injected cleanup failure');
    const engine = new RivetChatEngine(
      { ...testEnv(), ...READY_ENV },
      status,
      '/tmp/unused.js',
      createHostResourceOwner(),
      null,
      {
        startRegistry: async () => ({
          registry: {} as StartedRegistry['registry'],
          client: {} as StartedRegistry['client'],
          endpoint: 'http://127.0.0.1:8961',
          async shutdown() {
            calls += 1;
            if (calls === 1) throw failure;
          },
        }),
      },
    );
    const wrapper = new StoreProbeChatEngine({
      probe: async () => ({ reachable: true }),
      createReadyEngine: () => engine,
      initialEngine: engine,
    });
    await wrapper.start();
    await expect(wrapper.shutdown()).rejects.toBe(failure);
    expect(wrapper.endpoint).toBe('http://127.0.0.1:8961');
    await wrapper.shutdown();
    expect(calls).toBe(2);
    expect(wrapper.endpoint).toBeNull();
  });

  test('rejects a start after shutdown is terminal', async () => {
    let started = false;
    const engine = new RivetChatEngine(
      { ...testEnv(), ...READY_ENV },
      status,
      '/tmp/peri-wasm.js',
      createHostResourceOwner(),
      null,
      {
        startRegistry: async () => {
          started = true;
          throw new Error('shutdown is terminal');
        },
      },
    );

    await engine.shutdown();
    await expect(engine.start()).rejects.toMatchObject({ code: 'engine_unavailable' });
    expect(started).toBe(false);
  });
});
