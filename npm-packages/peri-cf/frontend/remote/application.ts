import { createRemoteActions } from "./actions";
import { ChatController, type ChatStateCache, type ChatView } from "./controller";
import { createHttpRepository } from "./repository";
import type { WebSocketLike } from "./sync";

export interface ChatApplicationOptions {
  /** API/WS 同源 base；测试可注入。 */
  origin?: string;
  cache?: ChatStateCache;
  webSocketFactory?: (url: string) => WebSocketLike;
  now?: () => Date;
}

export type { ChatView };

/**
 * 浏览器组合根：Agent 在服务端执行，前端只消费目录、Action 回执与只读 Yjs 副本。
 *
 * 没有本地 WASM 宿主、事件流订阅、localStorage 配置或旧业务门面。
 */
export function createChatApplication(options: ChatApplicationOptions = {}): ChatController {
  const base = options.origin ?? globalThis.location.origin;
  const repository = createHttpRepository(base);
  return new ChatController({
    actions: createRemoteActions({ origin: options.origin }),
    list: (signal) => repository.list(signal),
    syncUrl: (address) => new URL(
      `/api/chats/${encodeURIComponent(address)}/sync`, base,
    ).href.replace(/^http/, "ws"),
    ...(options.cache ? { cache: options.cache } : {}),
    ...(options.webSocketFactory ? { webSocketFactory: options.webSocketFactory } : {}),
    ...(options.now ? { now: options.now } : {}),
  });
}
