import type { HostResourceOwner } from '@peri-code/rivet/rivet';
import type { ChatEngine } from './chats/engine';
import type { ChatService } from './chats/service';
import { diagnosticCode } from './errors';
import type { SandboxRuntimeHandle } from './sandbox/runtime';

type Step = 'chats' | 'host' | 'sandbox' | 'engine';

class ShutdownTimeout extends Error {
  readonly code = 'shutdown_timeout';
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new ShutdownTimeout()), timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/** 清理调用在超时后仍归宿主持有，重试观察同一 Promise，而不是把「调用过」当完成。 */
export function createShutdown(options: {
  readonly stop: () => void;
  readonly chats: Pick<ChatService, 'closeAllChats'>;
  readonly owner: Pick<HostResourceOwner, 'beginShutdown' | 'closeAll'>;
  readonly sandbox: { readonly runtime: Pick<SandboxRuntimeHandle['runtime'], 'closeAll'> } | null;
  readonly engine: Pick<ChatEngine, 'shutdown'>;
  readonly timeoutMs?: number;
  readonly hostTimeoutMs?: number;
}) {
  const pending = new Map<Step, Promise<unknown>>();
  const completed = new Set<Step>();
  const failures = new Map<Step, unknown>();
  let running: Promise<{ complete: boolean; pending: Step[] }> | null = null;
  let stopped = false;

  async function step<T>(name: Step, operation: () => Promise<T>, timeout: number): Promise<T> {
    let promise = pending.get(name) as Promise<T> | undefined;
    if (promise === undefined) {
      promise = Promise.resolve().then(operation);
      pending.set(name, promise);
      // 注册观察者防止超时后的拒绝变成 unhandled；原 Promise 与 cause 仍保留。
      void promise.catch(() => undefined);
    }
    try {
      const result = await withTimeout(promise, timeout);
      pending.delete(name);
      failures.delete(name);
      return result;
    } catch (error) {
      failures.set(name, error);
      if (!(error instanceof ShutdownTimeout)) pending.delete(name);
      throw error;
    }
  }

  async function run() {
    if (!stopped) {
      stopped = true;
      options.stop();
    }
    const timeout = options.timeoutMs ?? 10_000;
    const hostTimeout = options.hostTimeoutMs ?? 5_000;
    let current: Step = 'chats';
    try {
      if (!completed.has('chats')) {
        const chats = await step('chats', () => options.chats.closeAllChats(), timeout);
        if (!chats.complete) return incomplete('chats');
        completed.add('chats');
      }
      current = 'host';
      options.owner.beginShutdown();
      if (!completed.has('host')) {
        const host = await step(
          'host',
          () => options.owner.closeAll({ timeoutMs: hostTimeout, maxAttempts: 2 }),
          hostTimeout + 1_000,
        );
        if (!host.complete) return incomplete('host');
        completed.add('host');
      }
      current = 'sandbox';
      if (!completed.has('sandbox')) {
        const sandbox = options.sandbox;
        if (sandbox !== null) {
          await step('sandbox', () => sandbox.runtime.closeAll(), hostTimeout);
          console.log('[agent-builder] sandbox runtime closed');
        }
        completed.add('sandbox');
      }
      current = 'engine';
      if (!completed.has('engine')) {
        await step('engine', () => options.engine.shutdown(), timeout);
        completed.add('engine');
      }
      return { complete: true, pending: [] };
    } catch (error) {
      console.error('[agent-builder] shutdown incomplete', {
        step: current,
        code: error instanceof ShutdownTimeout ? 'shutdown_timeout' : diagnosticCode(error),
      });
      return { complete: false, pending: [current] };
    }
  }

  function incomplete(step: Step) {
    console.error('[agent-builder] shutdown incomplete', { step });
    return { complete: false, pending: [step] };
  }

  return {
    run() {
      running ??= run().finally(() => {
        running = null;
      });
      return running;
    },
    get pending(): readonly Step[] {
      return [...pending.keys()];
    },
  };
}
