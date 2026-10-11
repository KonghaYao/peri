import { ChatRuntime } from "./session";
import type { ChatRepository, StartHost } from "./types";

/** 标签页内驻留的会话运行期上限：超限时按 LRU 回收空闲运行期。 */
export const MAX_RESIDENT_RUNTIMES = 8;

export interface RuntimeDeps {
  startHost: StartHost;
  repository: Pick<ChatRepository, "get">;
}

/**
 * 标签页内的会话运行期注册表：Map 的插入顺序即 LRU 顺序。
 * 运行期不跨标签页共享；未命中只表示本标签页需要重新复原。
 */
export class ChatRegistry {
  private readonly runtimes = new Map<string, ChatRuntime>();

  constructor(private readonly limit = MAX_RESIDENT_RUNTIMES) {}

  /** 取得会话运行期；不存在则创建（不触发复原，复原由首次 hydrate 决定）。 */
  runtimeFor(chatId: string, deps: RuntimeDeps): ChatRuntime {
    const existing = this.runtimes.get(chatId);
    if (existing) {
      this.touch(chatId, existing);
      return existing;
    }
    const runtime = new ChatRuntime(chatId, deps.startHost, deps.repository);
    this.runtimes.set(chatId, runtime);
    this.evict();
    return runtime;
  }

  /** 淘汰最久未用且空闲的运行期；全部忙碌时允许超限。 */
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
