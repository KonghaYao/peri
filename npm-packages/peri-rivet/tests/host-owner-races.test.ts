import { describe, expect, test } from 'bun:test';
import {
  createHostResourceOwner,
  createSandboxMcpActor,
  type HostResourceOwner,
  type SandboxActorResult,
  type SandboxMcpRuntimePort,
  type SandboxMcpStatusView,
} from '../src/rivet/index.js';
import {
  createSandboxVars,
  type SandboxActorContext,
  type SandboxActorVars,
} from '../src/rivet/sandbox-actor.js';
import { formatWorkspaceUri, type WorkspaceUri } from '../src/rivet/workspace-uri.js';

const MACHINE_ID = '00000000-0000-4000-8000-000000000001';
const WORKSPACE = formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: '/Users/me/owner-race' });
const HOLDER = 'sandbox-mcp:host';
const TOKEN = 'sandbox-owner-race-token-0123456789';

interface Definition {
  config: {
    createVars(): SandboxActorVars;
    actions: {
      acquire(
        c: SandboxActorContext,
        holder: string,
        credential: string,
      ): Promise<SandboxActorResult<{ workspaceUri: WorkspaceUri; state: 'ready' }>>;
      release(
        c: SandboxActorContext,
        holder: string,
        credential: string,
      ): Promise<SandboxActorResult<{ released: true }>>;
      status(c: SandboxActorContext, credential: string): Promise<SandboxActorResult<SandboxMcpStatusView>>;
      closeAll(c: SandboxActorContext, credential: string): Promise<SandboxActorResult<{ closed: true }>>;
    };
    onRequest(
      c: SandboxActorContext & { request?: Request | undefined },
      request: Request,
    ): Promise<Response>;
    onSleep(c: SandboxActorContext): Promise<void>;
    onDestroy(c: SandboxActorContext): Promise<void>;
  };
}

interface RecorderRuntime extends SandboxMcpRuntimePort {
  state: string | undefined;
  starts: number;
  startsAfterShutdown: number;
  closes: number;
}

function recorder(owner: HostResourceOwner): RecorderRuntime {
  const runtime: RecorderRuntime = {
    state: undefined,
    starts: 0,
    startsAfterShutdown: 0,
    closes: 0,
    async start() {
      runtime.starts += 1;
      if (owner.shuttingDown) {
        runtime.startsAfterShutdown += 1;
      }
      runtime.state = 'ready';
      return { state: 'ready' };
    },
    async close() {
      runtime.closes += 1;
      runtime.state = undefined;
    },
    stateOf: () => runtime.state,
    async fetchMcp() {
      return new Response(null);
    },
  };
  return runtime;
}

function definition(owner: HostResourceOwner, runtime: SandboxMcpRuntimePort): Definition {
  return createSandboxMcpActor({
    owner,
    runtime,
    token: TOKEN,
    machineId: MACHINE_ID,
    authorizedWorkspaces: [WORKSPACE],
  }) as unknown as Definition;
}

function context(
  key: string[] = ['sbx', WORKSPACE],
  vars: SandboxActorVars = createSandboxVars(),
): SandboxActorContext & { readonly active: () => number } {
  let active = 0;
  const c: SandboxActorContext = {
    vars,
    key,
    keepAwake: <T>(promise: Promise<T>): Promise<T> => {
      active += 1;
      return promise.finally(() => {
        active -= 1;
      });
    },
  };
  return Object.assign(c, { active: () => active });
}

const turn = (): Promise<void> => new Promise((resolve) => setImmediate(resolve));

describe('sandbox host owner races', () => {
  test('F2: acquire waiting for two microtasks cannot start after closeAll completes', async () => {
    const owner = createHostResourceOwner();
    const runtime = recorder(owner);
    const d = definition(owner, runtime);
    const c = context();

    const acquiring = d.config.actions.acquire(c, HOLDER, TOKEN);
    await Promise.resolve();
    await Promise.resolve();
    owner.beginShutdown();
    const [outcome, report] = await Promise.all([acquiring, owner.closeAll({ timeoutMs: 1_000 })]);
    await turn();

    expect(runtime.startsAfterShutdown).toBe(0);
    expect(runtime.state).toBeUndefined();
    expect(report.complete).toBe(true);
    expect(report.pending).toBe(0);
    expect(owner.pending()).toHaveLength(0);
    expect(c.vars.refs).toBe(0);
    if (outcome.ok) {
      // The acquire won before shutdown; its runtime must have been closed by closeAll.
      expect(outcome.value.state).toBe('ready');
      expect(runtime.closes).toBe(1);
    } else {
      expect(outcome.error.code).toBe('closed');
      expect(runtime.starts).toBe(0);
    }

    const startsBefore = runtime.starts;
    const late = await d.config.actions.acquire(c, HOLDER, TOKEN);
    expect(late.ok).toBe(false);
    if (!late.ok) {
      expect(late.error.code).toBe('closed');
    }
    expect(runtime.starts).toBe(startsBefore);
    await d.config.onDestroy(c);
    await turn();
    expect(c.active()).toBe(0);
    expect(runtime.state).toBeUndefined();
  });

  test('F2: microtask order matrix leaves no ready runtime after owner complete', async () => {
    for (let ticks = 0; ticks <= 8; ticks += 1) {
      const owner = createHostResourceOwner();
      const runtime = recorder(owner);
      const d = definition(owner, runtime);
      const c = context();

      const acquiring = d.config.actions.acquire(c, HOLDER, TOKEN);
      for (let index = 0; index < ticks; index += 1) {
        await Promise.resolve();
      }
      owner.beginShutdown();
      const [outcome, report] = await Promise.all([acquiring, owner.closeAll({ timeoutMs: 1_000 })]);
      await turn();

      expect({ ticks, complete: report.complete }).toEqual({ ticks, complete: true });
      expect({ ticks, pending: owner.pending().length }).toEqual({ ticks, pending: 0 });
      expect({ ticks, state: runtime.state }).toEqual({ ticks, state: undefined });
      expect({ ticks, startsAfterShutdown: runtime.startsAfterShutdown }).toEqual({
        ticks,
        startsAfterShutdown: 0,
      });
      expect({ ticks, refs: c.vars.refs }).toEqual({ ticks, refs: 0 });
      if (outcome.ok) {
        expect(runtime.starts).toBe(1);
        expect(runtime.closes).toBe(1);
      } else {
        expect(outcome.error.code).toBe('closed');
      }
      await d.config.onDestroy(c);
      await turn();
      expect(c.active()).toBe(0);
    }
  });

  test('F2/F3: an action whose lease retires while queued neither starts nor re-creates a hold', async () => {
    const owner = createHostResourceOwner();
    let state: string | undefined;
    let starts = 0;
    let releaseStart: (() => void) | undefined;
    let startEntered: (() => void) | undefined;
    const blocked = new Promise<void>((resolve) => {
      releaseStart = resolve;
    });
    const entered = new Promise<void>((resolve) => {
      startEntered = resolve;
    });
    const runtime: SandboxMcpRuntimePort = {
      async start() {
        starts += 1;
        startEntered?.();
        await blocked;
        state = 'ready';
        return { state: 'ready' };
      },
      async close() {
        state = undefined;
      },
      stateOf: () => state,
      async fetchMcp() {
        return new Response(null);
      },
    };
    const d = definition(owner, runtime);
    const c = context();
    const first = d.config.actions.acquire(c, 'first', TOKEN);
    await entered;
    const second = d.config.actions.acquire(c, 'second', TOKEN);
    await turn();
    owner.beginShutdown();
    const report = owner.closeAll({ timeoutMs: 1_000 });
    releaseStart?.();

    const [firstResult, secondResult, closed] = await Promise.all([first, second, report]);
    await turn();

    expect(firstResult.ok).toBe(true);
    expect(secondResult.ok).toBe(false);
    if (!secondResult.ok) {
      expect(secondResult.error.code).toBe('closed');
    }
    expect(closed.complete).toBe(true);
    expect(c.vars.refs).toBe(0);
    expect(state).toBeUndefined();
    expect(starts).toBe(1);
    expect(c.active()).toBe(0);
  });

  test('F2/F3: retire refuses acquire/release/status, detaches the hold, and host close owns the runtime', async () => {
    const owner = createHostResourceOwner();
    const runtime = recorder(owner);
    const d = definition(owner, runtime);
    const c = context();

    const acquired = await d.config.actions.acquire(c, HOLDER, TOKEN);
    expect(acquired.ok).toBe(true);
    expect(c.active()).toBe(1);
    const starts = runtime.starts;

    owner.retire(c.vars);
    await turn();
    expect(c.active()).toBe(0);

    for (const result of [
      await d.config.actions.acquire(c, HOLDER, TOKEN),
      await d.config.actions.release(c, HOLDER, TOKEN),
      await d.config.actions.status(c, TOKEN),
    ]) {
      expect(result.ok).toBe(false);
      if (!result.ok) {
        expect(result.error.code).toBe('closed');
      }
    }
    expect(runtime.starts).toBe(starts);
    expect(runtime.closes).toBe(0);

    const report = await owner.closeAll({ timeoutMs: 1_000 });
    expect(report.complete).toBe(true);
    expect(runtime.closes).toBe(1);
    expect(runtime.state).toBeUndefined();
    expect(c.vars.refs).toBe(0);
    expect(c.active()).toBe(0);
  });

  test('F3: replacing the same key detaches the old hold and keeps only the new hold', async () => {
    const owner = createHostResourceOwner();
    const runtime = recorder(owner);
    const d = definition(owner, runtime);
    const first = context();
    const second = context();

    await d.config.actions.acquire(first, HOLDER, TOKEN);
    expect(first.active()).toBe(1);

    const replaced = await d.config.actions.acquire(second, HOLDER, TOKEN);
    expect(replaced.ok).toBe(true);
    expect(first.vars.refs).toBe(0);
    expect(second.vars.refs).toBe(1);
    expect(first.active()).toBe(0);
    expect(second.active()).toBe(1);

    const released = await d.config.actions.release(second, HOLDER, TOKEN);
    expect(released.ok).toBe(true);
    await turn();
    expect(second.active()).toBe(0);
    expect(runtime.state).toBeUndefined();
    // Release closes the instance but the activation record stays host-owned until teardown/close.
    expect(owner.pending()).toHaveLength(1);
    const report = await owner.closeAll({ timeoutMs: 1_000 });
    expect(report.complete).toBe(true);
    expect(owner.pending()).toHaveLength(0);
  });

  test('F2: an action after sleep cannot reopen the activation on the same vars', async () => {
    const owner = createHostResourceOwner();
    const runtime = recorder(owner);
    const d = definition(owner, runtime);
    const c = context();
    await d.config.actions.acquire(c, HOLDER, TOKEN);

    await d.config.onSleep(c);
    expect(runtime.state).toBeUndefined();
    expect(c.vars.refs).toBe(0);
    const starts = runtime.starts;

    const reopened = await d.config.actions.acquire(c, HOLDER, TOKEN);
    expect(reopened.ok).toBe(false);
    if (!reopened.ok) {
      expect(reopened.error.code).toBe('closed');
    }
    expect(runtime.starts).toBe(starts);
    expect(runtime.state).toBeUndefined();
    expect(owner.pending()).toHaveLength(0);
    expect(c.active()).toBe(0);
  });

  test('F2: a retired record can still be closed through the closeAll action', async () => {
    const owner = createHostResourceOwner();
    const runtime = recorder(owner);
    const d = definition(owner, runtime);
    const c = context();
    await d.config.actions.acquire(c, HOLDER, TOKEN);

    owner.retire(c.vars);
    const closed = await d.config.actions.closeAll(c, TOKEN);
    expect(closed.ok).toBe(true);
    expect(runtime.closes).toBe(1);
    expect(runtime.state).toBeUndefined();
    expect(c.vars.refs).toBe(0);
    expect(owner.pending()).toHaveLength(0);
    expect(c.active()).toBe(0);
  });

  test('F2: retired vars stay closed after the closeAll action removed the record', async () => {
    const owner = createHostResourceOwner();
    const runtime = recorder(owner);
    const d = definition(owner, runtime);
    const c = context();
    expect((await d.config.actions.acquire(c, HOLDER, TOKEN)).ok).toBe(true);
    expect(runtime.starts).toBe(1);

    owner.retire(c.vars);
    await turn();
    const closed = await d.config.actions.closeAll(c, TOKEN);
    expect(closed.ok).toBe(true);
    expect(runtime.closes).toBe(1);
    expect(owner.pending()).toHaveLength(0);

    const reopened = await d.config.actions.acquire(c, HOLDER, TOKEN);
    expect(reopened.ok).toBe(false);
    if (!reopened.ok) {
      expect(reopened.error.code).toBe('closed');
    }
    expect(runtime.starts).toBe(1);
    expect(runtime.state).toBeUndefined();
    expect(c.vars.refs).toBe(0);
    expect(c.active()).toBe(0);
    expect(owner.pending()).toHaveLength(0);

    // A new activation for the same key is a different identity and may still open.
    const fresh = context();
    expect((await d.config.actions.acquire(fresh, HOLDER, TOKEN)).ok).toBe(true);
    expect(runtime.starts).toBe(2);
    expect(fresh.vars.refs).toBe(1);
    expect((await d.config.actions.release(fresh, HOLDER, TOKEN)).ok).toBe(true);
    const report = await owner.closeAll({ timeoutMs: 1_000 });
    expect(report.complete).toBe(true);
    await turn();
    expect(fresh.active()).toBe(0);
  });
});
