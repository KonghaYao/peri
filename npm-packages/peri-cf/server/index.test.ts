import { describe, expect, test } from "bun:test";
import {
  MemoryKV,
  type ChatDatabase,
  type ChatSessionDirectoryEntry,
} from "@peri-code/sdk";
import { publicServerInfo, type ServerConfig } from "./config";
import { AppChatManager, createSdkChatManager, type ChatPlatformConfig } from "./agent";
import { createServer } from ".";

const config: ServerConfig = {
  hostname: "127.0.0.1",
  port: 0,
  machineId: "11111111-1111-4111-8111-111111111111",
  model: {
    provider: "openai",
    id: "gpt-test",
    baseUrl: "https://api.example.com/v1",
    apiKey: "model-secret",
  },
  storage: { url: "https://store.example.com", authToken: "store-secret" },
  mcpServers: [],
  wasmModuleUrl: "file:///tmp/peri-wasm.js",
  staticRoot: "/tmp/peri-cf-missing-static",
};

const platform: ChatPlatformConfig = {
  machineId: config.machineId,
  model: config.model,
  storage: config.storage,
  mcpServers: config.mcpServers,
  wasmModuleUrl: config.wasmModuleUrl,
};

const directory: ChatSessionDirectoryEntry = {
  sessionId: "session-1",
  title: null,
  cwd: "/workspace/",
  messageCount: 1,
  createdAt: "2026-10-20T12:00:00Z",
  updatedAt: "2026-10-20T13:00:00Z",
};

const database: ChatDatabase = {
  async listSessions() { return [directory]; },
  async getSession() { return null; },
};

describe("server wiring", () => {
  test("HTTP、目录与 WS 410 路由在同一服务中工作", async () => {
    // 测试只装配 transport 宿主；不启动 WASM。
    const manager = new AppChatManager({
      chats: createSdkChatManager(platform, {
        database,
        sessionOwnershipKv: new MemoryKV(),
        chatConfig: {
          runtime: {
            kind: "transport",
            factory: async () => { throw new Error("not used"); },
          },
        },
      }),
    });
    const server = createServer({
      hostname: config.hostname,
      port: config.port,
      staticRoot: config.staticRoot,
      info: publicServerInfo(config),
    }, manager);
    try {
      const info = await fetch(`http://127.0.0.1:${server.port}/api/info`);
      expect(info.status).toBe(200);
      expect(await info.json()).toMatchObject({
        ok: true,
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
      await manager.shutdown();
    }
  });
});
