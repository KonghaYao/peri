import { describe, expect, test } from "bun:test";
import {
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatSessionDirectoryEntry,
} from "@peri-code/sdk";
import type { ServerConfig } from "./config";
import { createServer } from ".";
import { ChatRegistry } from "./registry";

const config: ServerConfig = {
  hostname: "127.0.0.1",
  port: 0,
  machineId: "11111111-1111-4111-8111-111111111111",
  model: {
    provider: "openai",
    tiers: { fable: "gpt-test", opus: "gpt-test", sonnet: "gpt-test", haiku: "gpt-test" },
    baseUrl: "https://api.example.com/v1",
    apiKey: "model-secret",
  },
  storage: { url: "https://store.example.com", authToken: "store-secret" },
  mcpServers: [],
  wasmModuleUrl: "file:///tmp/peri-wasm.js",
  staticRoot: "/tmp/peri-cf-missing-static",
};

const directory: ChatSessionDirectoryEntry = {
  sessionId: "session-1",
  title: null,
  cwd: "/workspace/",
  messageCount: 1,
  createdAt: "2026-10-20T12:00:00Z",
  updatedAt: "2026-10-20T13:00:00Z",
};

function stubChat(id: string): ChatServiceContract {
  return {
    id,
    sessionId: null,
    bindingRevision: 0,
    binding: null,
    execute(): never { throw new Error("stub chat cannot execute actions"); },
    readPayload(): Promise<never> { throw new Error("stub chat has no payload store"); },
    async close(): Promise<void> {},
    subscribeFacts() { return () => {}; },
  };
}

/** 与 HTTP 层交互所需的最小 Manager；不启动 WASM、不读 Store。 */
class FakeChatManager implements ChatManagerContract {
  private created = 0;

  createChat(): ChatServiceContract {
    return stubChat(`chat-${++this.created}`);
  }

  getChat(): ChatServiceContract | undefined {
    return undefined;
  }

  findChatBySession(): ChatServiceContract | undefined {
    return undefined;
  }

  async listSessions(): Promise<ChatSessionDirectoryEntry[]> {
    return [directory];
  }

  async closeChat(): Promise<void> {}

  async shutdown(): Promise<void> {}
}

describe("server wiring", () => {
  test("HTTP、目录与 WS 410 路由在同一服务中工作", async () => {
    const registry = new ChatRegistry(new FakeChatManager());
    const server = createServer(config, registry);
    try {
      const info = await fetch(`http://127.0.0.1:${server.port}/api/info`);
      expect(info.status).toBe(200);
      expect(await info.json()).toEqual({
        ok: true,
        model: {
          provider: "openai",
          tiers: { fable: "gpt-test", opus: "gpt-test", sonnet: "gpt-test", haiku: "gpt-test" },
          baseUrl: "https://api.example.com/v1",
        },
        storage: { url: "https://store.example.com/" },
        mcp: [],
        workspace: "/workspace",
        sessionCwd: "/workspace/",
      });

      const chats = await fetch(`http://127.0.0.1:${server.port}/api/chats`);
      expect(chats.status).toBe(200);
      expect(await chats.json()).toEqual({ chats: [directory], available: true });

      const missing = await fetch(
        `http://127.0.0.1:${server.port}/api/chats/missing/sync`,
        { headers: { upgrade: "websocket", connection: "Upgrade" } },
      );
      expect(missing.status).toBe(410);

      const created = await fetch(`http://127.0.0.1:${server.port}/api/chats`, {
        method: "POST",
      });
      expect(created.status).toBe(201);
      const createdBody = await created.json();
      expect(createdBody.chat.sessionId).toBeNull();
      expect(createdBody.chat.bindingRevision).toBe(0);
    } finally {
      server.stop(true);
      await registry.shutdown();
    }
  });
});
