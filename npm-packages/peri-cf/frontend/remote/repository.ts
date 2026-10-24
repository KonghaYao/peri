import type { Chat, ChatRepository, ChatSummary } from "@core/agent";
import { chatListSchema, chatSchema } from "@core/shared";

function apiUrl(base: string, path: string): string {
  return new URL(path, base || globalThis.location.origin).href;
}

async function json<T>(response: Response): Promise<T> {
  const body = await response.json().catch(() => null) as (T & { error?: string }) | null;
  if (!response.ok || body === null)
    throw new Error(body?.error ?? `请求失败（HTTP ${response.status}）`);
  return body;
}

/** 会话清单与创建走服务端 API；前端不再直接持有存储凭据。 */
export function createHttpRepository(base = ""): ChatRepository {
  return {
    async list(): Promise<ChatSummary[]> {
      const body = await json<{ chats: ChatSummary[] }>(await fetch(apiUrl(base, "/api/chats"), {
        headers: { accept: "application/json" },
      }));
      return chatListSchema.parse(body).chats;
    },

    async get(id: string): Promise<Chat | null> {
      const response = await fetch(apiUrl(base, `/api/chats/${encodeURIComponent(id)}`), {
        headers: { accept: "application/json" },
      });
      if (response.status === 404) return null;
      const body = await json<{ state: { chat: Chat } }>(response);
      return chatSchema.parse(body.state.chat);
    },

    async create(title: string): Promise<Chat> {
      const response = await fetch(apiUrl(base, "/api/chats"), {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify({ title }),
      });
      const body = await json<{ chat: Chat }>(response);
      return chatSchema.parse(body.chat);
    },
  };
}
