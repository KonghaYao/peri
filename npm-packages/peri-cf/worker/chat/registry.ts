import { ChatRuntime } from "./session";
import type { ChatRepository, Env, StartTransport } from "../types";

/** isolate 内驻留的会话运行期上限（建议值）：超限时按 LRU 回收空闲运行期。 */
export const MAX_RESIDENT_RUNTIMES = 8;

export interface RuntimeDeps {
  startTransport: StartTransport;
  repository: Pick<ChatRepository, "get">;
}

/**
 * isolate 内的会话运行期注册表：Map 的插入顺序即 LRU 顺序。
 * 运行期不跨 isolate 共享（无 DO），因此命中与否是本地事实；未命中只表示本 isolate 需要复原。
 */
export class ChatRegistry {
  private readonly runtimes = new Map<string, ChatRuntime>();

  constructor(private readonly limit = MAX_RESIDENT_RUNTIMES) {}

  /** 取得会话运行期；不存在则创建（不触发复原，复原由首次 hydrate 决定）。 */
  runtimeFor(chatId: string, env: Env, deps: RuntimeDeps): ChatRuntime {
    const existing = this.runtimes.get(chatId);
    if (existing) {
      this.touch(chatId, existing);
      return existing;
    }
    const runtime = new ChatRuntime(chatId, env, deps.startTransport, deps.repository);
    this.runtimes.set(chatId, runtime);
    this.evict();
    return runtime;
  }

  /** 只读探测：绝不创建、绝不 hydrate/重放（观测不得唤醒会话）。 */
  peek(chatId: string): ChatRuntime | undefined {
    const runtime = this.runtimes.get(chatId);
    if (runtime) this.touch(chatId, runtime);
    return runtime;
  }

  size(): number { return this.runtimes.size; }

  /** 淘汰最久未用且空闲（无活跃执行、无在连 peer）的运行期；全部忙碌时允许超限。 */
  private evict(): void {
    for (const [chatId, runtime] of [...this.runtimes]) {
      if (this.runtimes.size <= this.limit) return;
      if (!runtime.recyclable) continue;
      this.runtimes.delete(chatId);
      runtime.dispose();
    }
  }

  private touch(chatId: string, runtime: ChatRuntime): void {
    if (this.runtimes.get(chatId) !== runtime) return;
    this.runtimes.delete(chatId);
    this.runtimes.set(chatId, runtime);
  }
}

/** 部署实例的运行期注册表：跨请求存活于同一 isolate。 */
export const runtimes = new ChatRegistry();

export function runtimeFor(chatId: string, env: Env, deps: RuntimeDeps): ChatRuntime {
  return runtimes.runtimeFor(chatId, env, deps);
}

export function peekRuntime(chatId: string): ChatRuntime | undefined {
  return runtimes.peek(chatId);
}
