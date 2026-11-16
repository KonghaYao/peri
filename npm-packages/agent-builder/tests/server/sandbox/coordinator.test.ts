import { describe, expect, test } from 'bun:test';
import { AppError } from '../../../server/errors';
import { SandboxCoordinator } from '../../../server/sandbox/coordinator';
import { FakeSandboxActor } from '../chats/helpers';
import { testEnv } from '../helpers';

const TOKEN = 'sandbox-actor-token-0123456789';

function enabledEnv() {
  return testEnv({
    sandbox: {
      enabled: true,
      mode: 'ready',
      reasons: [],
      periPath: '/tmp/peri',
      settingsPath: undefined,
      sandboxesRoot: '/tmp/agent-builder-sandboxes',
      profileName: 'default',
      token: TOKEN,
      gatewayToken: 'default-gateway',
      startupTimeoutMs: 90_000,
      stopTimeoutMs: 20_000,
      dataDir: '/tmp',
      workspacePathPrivate: true,
    },
  });
}

function engine(actor: FakeSandboxActor) {
  return {
    endpoint: 'http://127.0.0.1:8961',
    actor: () => actor,
  };
}

describe('SandboxCoordinator', () => {
  test('acquires once per chat and injects the actor secret only as a header', async () => {
    const env = enabledEnv();
    const actor = new FakeSandboxActor(env.workspaceUri);
    actor.actorId = 'actor id/with slash';
    const coordinator = new SandboxCoordinator(env, engine(actor));

    const injection = await coordinator.acquire('chat-1');
    expect(injection).not.toBeNull();
    if (injection === null) return;
    expect(actor.acquireCalls).toEqual([{ holder: 'chat-1', credential: TOKEN }]);
    expect(injection.url).toBe('http://127.0.0.1:8961/gateway/actor%20id%2Fwith%20slash/request/mcp');
    expect(injection.url).not.toContain(TOKEN);
    expect(injection.headers['x-peri-sandbox-token']).toBe(TOKEN);
    expect(injection.headers['x-rivet-token']).toBe('default-gateway');
    expect(coordinator.state('chat-1')).toEqual({ state: 'ready', reason: null });

    // Cached: a second send must not acquire a second holder.
    expect(await coordinator.acquire('chat-1')).toBe(injection);
    expect(actor.acquireCalls).toHaveLength(1);
  });

  test('keeps per-chat holders independent on the same workspace', async () => {
    const env = enabledEnv();
    const actor = new FakeSandboxActor(env.workspaceUri);
    const coordinator = new SandboxCoordinator(env, engine(actor));

    await coordinator.acquire('chat-1');
    await coordinator.acquire('chat-2');
    expect(actor.acquireCalls.map((call) => call.holder)).toEqual(['chat-1', 'chat-2']);

    const first = await coordinator.release('chat-1');
    expect(first.released).toBe(true);
    expect(actor.releaseCalls.map((call) => call.holder)).toEqual(['chat-1']);
    expect(coordinator.state('chat-1').state).toBe('closed');
    expect(coordinator.state('chat-2').state).toBe('ready');

    // Release is idempotent and must not call the actor twice.
    expect(await coordinator.release('chat-1')).toEqual({ released: true, errors: [] });
    expect(actor.releaseCalls).toHaveLength(1);
  });

  test('an unconfirmed release keeps the holder and reports only a safe code', async () => {
    const env = enabledEnv();
    const actor = new FakeSandboxActor(env.workspaceUri);
    const coordinator = new SandboxCoordinator(env, engine(actor));
    await coordinator.acquire('chat-1');

    actor.failRelease = { code: 'sandbox_close_incomplete', message: 'token=secret /path' };
    const failed = await coordinator.release('chat-1');
    expect(failed).toEqual({ released: false, errors: ['sandbox_close_incomplete'] });
    expect(JSON.stringify(failed)).not.toContain('token=secret');
    expect(coordinator.state('chat-1')).toEqual({
      state: 'incomplete',
      reason: 'sandbox_close_incomplete',
    });

    // Retrying acquire first retries the old release; it must not stack a second holder.
    const retried = await coordinator.acquire('chat-1');
    expect(retried).not.toBeNull();
    expect(actor.releaseCalls).toHaveLength(2);
    expect(actor.acquireCalls).toHaveLength(2);
  });

  test('maps a start failure to a retryable engine error without leaking actor text', async () => {
    const env = enabledEnv();
    const actor = new FakeSandboxActor(env.workspaceUri);
    const coordinator = new SandboxCoordinator(env, engine(actor));
    actor.failAcquire = { code: 'sandbox_start_failed', message: '/Users/secret token=abc' };

    const error = await coordinator.acquire('chat-1').catch((value: unknown) => value);
    expect(error).toBeInstanceOf(AppError);
    expect((error as AppError).code).toBe('engine_unavailable');
    expect((error as AppError).retryable).toBe(true);
    expect((error as AppError).details).toEqual({ reason: 'sandbox_start_failed' });
    expect(JSON.stringify((error as AppError).toBody())).not.toContain('token=abc');
    expect(coordinator.state('chat-1')).toEqual({ state: 'error', reason: 'sandbox_start_failed' });
  });

  test('closeAll releases every held chat and reports incomplete progress', async () => {
    const env = enabledEnv();
    const actor = new FakeSandboxActor(env.workspaceUri);
    const coordinator = new SandboxCoordinator(env, engine(actor));
    await coordinator.acquire('chat-1');
    await coordinator.acquire('chat-2');

    expect(await coordinator.closeAll()).toEqual({ released: true, errors: [] });
    expect(actor.releaseCalls.map((call) => call.holder).sort()).toEqual(['chat-1', 'chat-2']);
    expect(coordinator.state('chat-1').state).toBe('closed');
    expect(coordinator.state('chat-2').state).toBe('closed');
  });

  test('disabled sandbox is explicit and never contacts the engine', async () => {
    const env = testEnv();
    let called = false;
    const coordinator = new SandboxCoordinator(env, {
      endpoint: 'http://127.0.0.1:8961',
      actor: () => {
        called = true;
        throw new Error('must not be called');
      },
    });

    expect(await coordinator.acquire('chat-1')).toBeNull();
    expect(await coordinator.release('chat-1')).toEqual({ released: true, errors: [] });
    expect(coordinator.state('chat-1')).toEqual({ state: 'disabled', reason: null });
    expect(called).toBe(false);
  });

  test('a misconfigured enabled sandbox blocks before any holder is created', () => {
    const env = testEnv({
      sandbox: {
        ...testEnv().sandbox,
        enabled: true,
        mode: 'misconfigured',
        reasons: ['sandbox_peri_missing'],
      },
    });
    const coordinator = new SandboxCoordinator(env, null);
    expect(() => coordinator.acquire('chat-1')).toThrow(
      expect.objectContaining({ code: 'not_ready', retryable: false }),
    );
  });
});
