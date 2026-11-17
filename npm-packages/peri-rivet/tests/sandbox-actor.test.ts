import { describe, expect, test } from 'bun:test';
import type { SandboxActorDiagnostic, SandboxActorOptions, SandboxActorResult } from '../src/rivet/index.js';
import {
  createSandboxActor,
  createSandboxVars,
  type SandboxAcquireView,
  type SandboxActorContext,
  SandboxActorCore,
  type SandboxActorVars,
  type SandboxStatusView,
} from '../src/rivet/sandbox-actor.js';

const SANDBOX_ID = 'sbx_0123456789abcdef0123456789abcdef';
const OTHER_ID = 'sbx_fedcba9876543210fedcba9876543210';

class FakeRuntime {
  readonly started: string[] = [];
  readonly closed: string[] = [];
  readonly states = new Map<string, string>();
  failStart: unknown = undefined;
  failClose: unknown = undefined;

  async start(sandboxId: string): Promise<unknown> {
    if (this.failStart !== undefined) {
      throw this.failStart;
    }
    this.started.push(sandboxId);
    this.states.set(sandboxId, 'ready');
    return { sandboxId };
  }

  async close(sandboxId: string): Promise<void> {
    if (this.failClose !== undefined) {
      throw this.failClose;
    }
    this.closed.push(sandboxId);
    this.states.delete(sandboxId);
  }

  stateOf(sandboxId: string): string | undefined {
    return this.states.get(sandboxId);
  }
}

interface TestActions {
  acquire(c: SandboxActorContext, holder: string): Promise<SandboxActorResult<SandboxAcquireView>>;
  release(c: SandboxActorContext, holder: string): Promise<SandboxActorResult<{ readonly released: true }>>;
  status(c: SandboxActorContext): Promise<SandboxActorResult<SandboxStatusView>>;
  closeAll(c: SandboxActorContext): Promise<SandboxActorResult<{ readonly closed: true }>>;
}

interface SandboxDefinition {
  config: {
    createVars(): SandboxActorVars;
    actions: TestActions;
    onSleep?(c: SandboxActorContext): void | Promise<void>;
    onDestroy?(c: SandboxActorContext): void | Promise<void>;
  };
}

function actorDefinition(options: SandboxActorOptions = {}): SandboxDefinition {
  return createSandboxActor(options) as unknown as SandboxDefinition;
}

function context(vars = createSandboxVars(), key: string[] = ['sbx', SANDBOX_ID]) {
  const kept: Promise<unknown>[] = [];
  const c: SandboxActorContext = {
    vars,
    key,
    keepAwake<T>(promise: Promise<T>): Promise<T> {
      kept.push(promise);
      return promise;
    },
  };
  return { c, kept };
}

function diagnostics(): {
  events: SandboxActorDiagnostic[];
  sink: (diagnostic: SandboxActorDiagnostic) => void;
} {
  const events: SandboxActorDiagnostic[] = [];
  return { events, sink: (diagnostic) => events.push(diagnostic) };
}

async function settled(promise: Promise<unknown> | undefined): Promise<boolean> {
  if (promise === undefined) {
    return false;
  }
  return await Promise.race([promise.then(() => true), Bun.sleep(20).then(() => false)]);
}

describe('SandboxActorCore', () => {
  test('acquire 登记 holder、refs 与 startedAtMs；同一 holder 重复 acquire 不重复计数', async () => {
    const runtime = new FakeRuntime();
    const vars = createSandboxVars();
    const core = new SandboxActorCore(SANDBOX_ID, vars, { runtime, now: () => 123 });

    expect(await core.acquire('agent:agt_1')).toEqual({
      ok: true,
      value: { sandboxId: SANDBOX_ID, state: 'ready' },
    });
    expect(vars).toEqual({ refs: 1, holders: ['agent:agt_1'], startedAtMs: 123 });

    await core.acquire('agent:agt_1');
    expect(vars.refs).toBe(1);
    expect(vars.holders).toEqual(['agent:agt_1']);
    expect(vars.startedAtMs).toBe(123);
    // 引用仍在时重复 acquire 只做幂等的实例确认。
    expect(runtime.started).toEqual([SANDBOX_ID, SANDBOX_ID]);
  });

  test('多个 holder 共享实例；最后一个 release 才 close 并重置', async () => {
    const runtime = new FakeRuntime();
    const vars = createSandboxVars();
    const core = new SandboxActorCore(SANDBOX_ID, vars, { runtime, now: () => 1 });

    await core.acquire('chat:lease-1');
    await core.acquire('agent:agt_1');
    expect(vars.refs).toBe(2);
    expect(runtime.started).toEqual([SANDBOX_ID, SANDBOX_ID]);

    await core.release('chat:lease-1');
    expect(vars.refs).toBe(1);
    expect(runtime.closed).toEqual([]);

    await core.release('agent:agt_1');
    expect(runtime.closed).toEqual([SANDBOX_ID]);
    expect(vars).toEqual({ refs: 0, holders: [], startedAtMs: null });
  });

  test('release 未知 holder 是成功 no-op', async () => {
    const runtime = new FakeRuntime();
    const vars = createSandboxVars();
    const core = new SandboxActorCore(SANDBOX_ID, vars, { runtime });
    await core.acquire('agent:agt_1');

    expect(await core.release('agent:missing')).toEqual({ ok: true, value: { released: true } });
    expect(vars).toEqual({ refs: 1, holders: ['agent:agt_1'], startedAtMs: expect.any(Number) });
    expect(runtime.closed).toEqual([]);
  });

  test('release 到零时 close 抛错 → sandbox_close_incomplete 且保留引用，可重试', async () => {
    const runtime = new FakeRuntime();
    const vars = createSandboxVars();
    const core = new SandboxActorCore(SANDBOX_ID, vars, { runtime });
    await core.acquire('agent:agt_1');

    runtime.failClose = new Error('close failed');
    const failed = await core.release('agent:agt_1');
    expect(failed.ok).toBe(false);
    if (!failed.ok) {
      expect(failed.error).toMatchObject({
        code: 'sandbox_close_incomplete',
        phase: 'sandbox',
        retryable: true,
      });
      expect(failed.error.message).not.toContain('close failed');
    }
    expect(vars.holders).toEqual(['agent:agt_1']);
    expect(vars.refs).toBe(1);

    runtime.failClose = undefined;
    expect(await core.release('agent:agt_1')).toEqual({ ok: true, value: { released: true } });
    expect(runtime.closed).toEqual([SANDBOX_ID]);
    expect(vars.refs).toBe(0);
  });

  test('release 关闭期间并发的 acquire 排队执行，新持有者不会丢失', async () => {
    const runtime = new FakeRuntime();
    let finishClose!: () => void;
    const closing = new Promise<void>((resolve) => {
      finishClose = resolve;
    });
    const close = runtime.close.bind(runtime);
    runtime.close = async (sandboxId: string) => {
      await closing;
      await close(sandboxId);
    };
    const vars = createSandboxVars();
    const core = () => new SandboxActorCore(SANDBOX_ID, vars, { runtime, now: () => 5 });

    await core().acquire('chat:old');
    const releasing = core().release('chat:old');
    const acquiring = core().acquire('chat:new');

    // close 未完成前 acquire 被队列挡住，不得登记新 holder。
    await Bun.sleep(5);
    expect(vars.holders).toEqual(['chat:old']);
    expect(vars.refs).toBe(1);

    finishClose();
    expect(await acquiring).toEqual({ ok: true, value: { sandboxId: SANDBOX_ID, state: 'ready' } });
    await releasing;
    expect(vars).toEqual({ refs: 1, holders: ['chat:new'], startedAtMs: 5 });
    expect(runtime.states.get(SANDBOX_ID)).toBe('ready');
  });

  test('status 校正漂移：refs>0 但实例已丢失时重置，且不调用 close', async () => {
    const runtime = new FakeRuntime();
    const vars = createSandboxVars();
    const core = new SandboxActorCore(SANDBOX_ID, vars, { runtime, now: () => 7 });
    await core.acquire('agent:agt_1');

    runtime.states.clear();
    expect(await core.status()).toEqual({
      ok: true,
      value: {
        sandboxId: SANDBOX_ID,
        present: false,
        state: null,
        refs: 0,
        holders: [],
        startedAtMs: null,
        orphaned: false,
      },
    });
    expect(vars.refs).toBe(0);
    expect(runtime.closed).toEqual([]);
  });

  test('status.orphaned：refs=0 但实例仍在 → 上报孤儿且不销毁', async () => {
    const runtime = new FakeRuntime();
    runtime.states.set(SANDBOX_ID, 'ready');
    const vars = createSandboxVars();
    const core = new SandboxActorCore(SANDBOX_ID, vars, { runtime });

    const status = await core.status();
    expect(status.ok && status.value).toMatchObject({
      present: true,
      state: 'ready',
      refs: 0,
      orphaned: true,
    });
    expect(runtime.closed).toEqual([]);
    expect(runtime.states.get(SANDBOX_ID)).toBe('ready');
  });

  test('closeAll 只关闭本 sandboxId，不影响其它实例', async () => {
    const runtime = new FakeRuntime();
    runtime.states.set(OTHER_ID, 'ready');
    const vars = createSandboxVars();
    const core = new SandboxActorCore(SANDBOX_ID, vars, { runtime });
    await core.acquire('agent:agt_1');

    expect(await core.closeAll()).toEqual({ ok: true, value: { closed: true } });
    expect(runtime.closed).toEqual([SANDBOX_ID]);
    expect(runtime.states.get(OTHER_ID)).toBe('ready');
    expect(vars).toEqual({ refs: 0, holders: [], startedAtMs: null });
  });

  test('closeAll 清理 refs=0 的孤儿实例；close 失败时保留引用', async () => {
    const runtime = new FakeRuntime();
    runtime.states.set(SANDBOX_ID, 'ready');
    const vars = createSandboxVars();
    const core = new SandboxActorCore(SANDBOX_ID, vars, { runtime });

    expect(await core.closeAll()).toEqual({ ok: true, value: { closed: true } });
    expect(runtime.closed).toEqual([SANDBOX_ID]);

    await core.acquire('agent:agt_1');
    runtime.failClose = new Error('close failed');
    const failed = await core.closeAll();
    expect(failed.ok).toBe(false);
    if (!failed.ok) {
      expect(failed.error.code).toBe('sandbox_close_incomplete');
    }
    expect(vars.holders).toEqual(['agent:agt_1']);
  });
});

describe('createSandboxActor actions', () => {
  test('未配置 runtime：acquire 返回 sandbox_unavailable，release/status/closeAll 仍成功', async () => {
    const { events, sink } = diagnostics();
    const definition = actorDefinition({ onDiagnostic: sink });
    const { c } = context();

    const acquired = await definition.config.actions.acquire(c, 'agent:agt_1');
    expect(acquired.ok).toBe(false);
    if (!acquired.ok) {
      expect(acquired.error).toMatchObject({
        name: 'PeriError',
        code: 'sandbox_unavailable',
        phase: 'sandbox',
        message: 'Sandbox pool is not configured',
      });
    }
    expect(events).toEqual([{ type: 'runtime_missing', sandboxId: SANDBOX_ID }]);

    expect(await definition.config.actions.release(c, 'agent:agt_1')).toEqual({
      ok: true,
      value: { released: true },
    });
    const status = await definition.config.actions.status(c);
    expect(status.ok && status.value).toMatchObject({ present: false, state: null, refs: 0 });
    expect(await definition.config.actions.closeAll(c)).toEqual({
      ok: true,
      value: { closed: true },
    });
  });

  test('start 抛错 → sandbox_start_failed，holder 未登记，原文只进诊断', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    const definition = actorDefinition({ runtime, onDiagnostic: sink });
    const { c } = context();

    const secret = new Error('secret path /tmp/leak');
    runtime.failStart = secret;
    const result = await definition.config.actions.acquire(c, 'agent:agt_1');
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toMatchObject({
        code: 'sandbox_start_failed',
        phase: 'sandbox',
        message: `Sandbox ${SANDBOX_ID} failed to start`,
      });
      expect(JSON.stringify(result.error)).not.toContain('secret path');
    }
    expect(c.vars).toEqual({ refs: 0, holders: [], startedAtMs: null });
    expect(events).toEqual([{ type: 'start_failed', sandboxId: SANDBOX_ID, error: secret }]);
  });

  test('close 抛错不吞：信封为 sandbox_close_incomplete，诊断保留原始 error', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    const definition = actorDefinition({ runtime, onDiagnostic: sink });
    const { c } = context();
    await definition.config.actions.acquire(c, 'agent:agt_1');

    const secret = new Error('adapter close failed');
    runtime.failClose = secret;
    const released = await definition.config.actions.release(c, 'agent:agt_1');
    expect(released.ok).toBe(false);
    if (!released.ok) {
      expect(released.error.code).toBe('sandbox_close_incomplete');
      expect(released.error.message).not.toContain('adapter close failed');
    }
    expect(events).toEqual([{ type: 'close_failed', sandboxId: SANDBOX_ID, error: secret }]);
  });

  test('key 非法（缺 key[1]）→ invalid_identity，不触达 runtime', async () => {
    const runtime = new FakeRuntime();
    const definition = actorDefinition({ runtime });
    const { c } = context(createSandboxVars(), ['sbx']);

    const result = await definition.config.actions.acquire(c, 'agent:agt_1');
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toMatchObject({ code: 'invalid_identity', phase: 'input' });
    }
    expect(runtime.started).toEqual([]);
  });

  test('key 前缀不是 sbx → invalid_identity，不触达 runtime', async () => {
    const runtime = new FakeRuntime();
    const definition = actorDefinition({ runtime });
    const { c } = context(createSandboxVars(), ['wrong-prefix', SANDBOX_ID]);

    const result = await definition.config.actions.acquire(c, 'agent:agt_1');
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toMatchObject({
        code: 'invalid_identity',
        phase: 'input',
        details: { reason: 'actor_key_shape', keyLength: 2 },
      });
    }
    expect(runtime.started).toEqual([]);
    expect(runtime.states.get(SANDBOX_ID)).toBeUndefined();
  });

  test('key 长度不是 2 → acquire/status/closeAll 均 invalid_identity，不触达 runtime', async () => {
    const runtime = new FakeRuntime();
    const definition = actorDefinition({ runtime });

    for (const key of [['sbx'], ['sbx', SANDBOX_ID, 'extra']]) {
      const { c } = context(createSandboxVars(), [...key]);
      const acquired = await definition.config.actions.acquire(c, 'agent:agt_1');
      expect(acquired.ok).toBe(false);
      if (!acquired.ok) {
        expect(acquired.error).toMatchObject({
          code: 'invalid_identity',
          phase: 'input',
          details: { reason: 'actor_key_shape', keyLength: key.length },
        });
      }
      expect((await definition.config.actions.status(c)).ok).toBe(false);
      expect((await definition.config.actions.closeAll(c)).ok).toBe(false);
    }
    expect(runtime.started).toEqual([]);
    expect(runtime.closed).toEqual([]);
  });

  test('正确 key ["sbx", sandboxId]：acquire / status / closeAll 正常', async () => {
    const runtime = new FakeRuntime();
    const definition = actorDefinition({ runtime, now: () => 3 });
    const { c } = context();

    expect(await definition.config.actions.acquire(c, 'agent:agt_1')).toEqual({
      ok: true,
      value: { sandboxId: SANDBOX_ID, state: 'ready' },
    });
    const status = await definition.config.actions.status(c);
    expect(status.ok && status.value).toMatchObject({
      sandboxId: SANDBOX_ID,
      present: true,
      state: 'ready',
      refs: 1,
      holders: ['agent:agt_1'],
    });
    expect(await definition.config.actions.closeAll(c)).toEqual({ ok: true, value: { closed: true } });
    expect(runtime.closed).toEqual([SANDBOX_ID]);
    expect(c.vars.refs).toBe(0);
  });

  test('错误前缀的 key 不能 acquire 或 closeAll 正当 key 持有的同一 sandboxId', async () => {
    const runtime = new FakeRuntime();
    const definition = actorDefinition({ runtime });
    const valid = context();
    await definition.config.actions.acquire(valid.c, 'agent:agt_1');
    expect(runtime.states.get(SANDBOX_ID)).toBe('ready');

    const wrong = context(createSandboxVars(), ['wrong-prefix', SANDBOX_ID]);
    expect((await definition.config.actions.acquire(wrong.c, 'chat:rt_1')).ok).toBe(false);
    expect((await definition.config.actions.closeAll(wrong.c)).ok).toBe(false);

    expect(valid.c.vars.refs).toBe(1);
    expect(runtime.states.get(SANDBOX_ID)).toBe('ready');
    expect(runtime.closed).toEqual([]);
  });

  test('holder 非法 → invalid_identity，不触达 runtime', async () => {
    const runtime = new FakeRuntime();
    const definition = actorDefinition({ runtime });
    const { c } = context();

    const result = await definition.config.actions.acquire(c, 'agent: bad holder');
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.code).toBe('invalid_identity');
    }
    expect(runtime.started).toEqual([]);
  });

  test('keepAwake：refs>0 只持有一个 hold，多 holder 不重复，归零释放', async () => {
    const runtime = new FakeRuntime();
    const definition = actorDefinition({ runtime });
    const { c, kept } = context();

    await definition.config.actions.acquire(c, 'chat:lease-1');
    expect(kept).toHaveLength(1);
    await definition.config.actions.acquire(c, 'agent:agt_1');
    expect(kept).toHaveLength(1);

    await definition.config.actions.release(c, 'chat:lease-1');
    expect(kept).toHaveLength(1);
    expect(await settled(kept[0])).toBe(false);

    await definition.config.actions.release(c, 'agent:agt_1');
    expect(await settled(kept[0])).toBe(true);
  });

  test('keepAwake 拒绝 → keep_awake_failed 诊断，动作仍成功', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    const definition = actorDefinition({ runtime, onDiagnostic: sink });
    const failure = new Error('keep awake rejected');
    const vars = createSandboxVars();
    const c: SandboxActorContext = {
      vars,
      key: ['sbx', SANDBOX_ID],
      keepAwake<T>(): Promise<T> {
        return Promise.reject(failure);
      },
    };

    const acquired = await definition.config.actions.acquire(c, 'agent:agt_1');
    expect(acquired.ok).toBe(true);
    await Bun.sleep(0);
    expect(events).toEqual([{ type: 'keep_awake_failed', sandboxId: SANDBOX_ID, error: failure }]);
  });

  test('诊断回调抛错不影响动作结果', async () => {
    const runtime = new FakeRuntime();
    const definition = actorDefinition({
      runtime,
      onDiagnostic: () => {
        throw new Error('observer failed');
      },
    });
    const { c } = context();

    expect((await definition.config.actions.acquire(c, 'agent:agt_1')).ok).toBe(true);
    expect((await definition.config.actions.status(c)).ok).toBe(true);
    expect((await definition.config.actions.closeAll(c)).ok).toBe(true);
  });
});

describe('sandbox actor lifecycle', () => {
  test('onSleep/onDestroy：refs>0 上报 holders_lost_on_teardown 并关闭本实例', async () => {
    const runtime = new FakeRuntime();
    runtime.states.set(OTHER_ID, 'ready');
    const { events, sink } = diagnostics();
    const definition = actorDefinition({ runtime, onDiagnostic: sink });
    const { c, kept } = context();
    await definition.config.actions.acquire(c, 'agent:agt_1');

    await definition.config.onSleep?.(c);
    expect(events).toEqual([{ type: 'holders_lost_on_teardown', sandboxId: SANDBOX_ID, refs: 1 }]);
    expect(runtime.closed).toEqual([SANDBOX_ID]);
    expect(runtime.states.get(OTHER_ID)).toBe('ready');
    expect(c.vars.refs).toBe(0);
    expect(await settled(kept[0])).toBe(true);
  });

  test('onDestroy：close 抛错不抛出，只记录诊断', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    const definition = actorDefinition({ runtime, onDiagnostic: sink });
    const { c } = context();
    await definition.config.actions.acquire(c, 'agent:agt_1');

    const failure = new Error('close failed');
    runtime.failClose = failure;
    await expect(definition.config.onDestroy?.(c)).resolves.toBeUndefined();
    expect(events).toEqual([
      { type: 'holders_lost_on_teardown', sandboxId: SANDBOX_ID, refs: 1 },
      { type: 'close_failed', sandboxId: SANDBOX_ID, error: failure },
    ]);
  });
});
