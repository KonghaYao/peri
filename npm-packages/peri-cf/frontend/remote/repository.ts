import { chatDirectoryResponseSchema, type ChatSummary } from "@core/shared";

/** 目录不可达：保留明确失败，不返回伪造的空列表。 */
export class ChatDirectoryUnavailableError extends Error {
  constructor(message: string, readonly status: number) {
    super(message);
    this.name = "ChatDirectoryUnavailableError";
  }
}

export interface ChatDirectoryRepository {
  list(signal?: AbortSignal): Promise<ChatSummary[]>;
}

/**
 * 会话目录：只读 `GET /api/chats`。
 *
 * 运行态不来自列表；条目只保留 Store 能提供的会话事实。创建由
 * `actions.createHandle` 负责，不在这里伪造标题或运行状态。
 */
export function createHttpRepository(base = "", fetchImpl: typeof fetch = globalThis.fetch): ChatDirectoryRepository {
  return {
    async list(signal?: AbortSignal): Promise<ChatSummary[]> {
      let response: Response;
      try {
        response = await fetchImpl(new URL("/api/chats", base || globalThis.location.origin).href, {
          headers: { accept: "application/json" }, signal,
        });
      } catch (error) {
        if (signal?.aborted) throw error;
        throw new ChatDirectoryUnavailableError(
          error instanceof Error ? error.message : "会话目录请求失败。", 0);
      }
      const body = await response.json().catch(() => undefined);
      const parsed = chatDirectoryResponseSchema.safeParse(body);
      if (!response.ok || !parsed.success || !parsed.data.available) {
        const message = parsed.success && parsed.data.error
          ? parsed.data.error
          : `会话目录不可用（HTTP ${response.status}）。`;
        throw new ChatDirectoryUnavailableError(message, response.status);
      }
      return parsed.data.chats;
    },
  };
}
