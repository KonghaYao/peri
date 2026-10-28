import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  type ChatAgentConfig,
  type ChatCreateOptions,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatSessionDirectoryEntry,
} from "@peri-code/sdk";
import type { ServerConfig } from "./config";
import { createServer } from ".";
import { defaultWorkspace } from "./compose";
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
  sandbox: { enabled: false, root: "/tmp/peri-sandbox" },
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

/** 进程级 agent 模板；服务接线测试不启动 WASM，只需形状合法。 */
const AGENT: ChatAgentConfig = {};

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

  createChat(_options: ChatCreateOptions): ChatServiceContract {
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
    const registry = new ChatRegistry(
      new FakeChatManager(),
      defaultWorkspace(config),
      AGENT,
      {
        session: { instructions: "test", mcpServers: {} },
        sessions: { getSession: async () => null },
      },
    );
    const server = createServer(config, registry);
    try {
      const monitor = await fetch(`http://127.0.0.1:${server.port}/api/monitor`);
      expect(monitor.status).toBe(200);
      const monitorBody = await monitor.json();
      expect(monitorBody).toMatchObject({
        ok: true,
        llm: {
          provider: "openai",
          activeAlias: "sonnet",
          tiers: { fable: "gpt-test", opus: "gpt-test", sonnet: "gpt-test", haiku: "gpt-test" },
          baseUrl: "https://api.example.com/v1",
        },
        mcp: { sandboxEnabled: false, remote: [], sandbox: [] },
        wasm: { activeChats: 0 },
        sandbox: {
          enabled: false,
          settings: { source: "disabled" },
          workspacesOnDisk: null,
          active: [],
          starting: 0,
        },
      });
      expect(typeof monitorBody.generatedAt).toBe("string");
      expect(typeof monitorBody.wasm.process.rssBytes).toBe("number");
      expect(JSON.stringify(monitorBody)).not.toContain("model-secret");
      expect(JSON.stringify(monitorBody)).not.toContain("store-secret");
      expect((await fetch(`http://127.0.0.1:${server.port}/api/info`)).status).toBe(404);

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

  test("Hono 提供静态资源、SPA 回退，并隔离未匹配的 API", async () => {
    const root = mkdtempSync(join(tmpdir(), "peri-cf-static-"));
    writeFileSync(join(root, "index.html"), "<h1>peri</h1>");
    writeFileSync(join(root, "asset.txt"), "asset-body");
    const registry = new ChatRegistry(
      new FakeChatManager(),
      defaultWorkspace(config),
      AGENT,
      {
        session: { instructions: "test", mcpServers: {} },
        sessions: { getSession: async () => null },
      },
    );
    const server = createServer({ ...config, staticRoot: root }, registry);
    try {
      const index = await fetch(`http://127.0.0.1:${server.port}/`);
      expect(index.status).toBe(200);
      expect(await index.text()).toContain("peri");

      const head = await fetch(`http://127.0.0.1:${server.port}/`, { method: "HEAD" });
      expect(head.status).toBe(200);
      expect(await head.text()).toBe("");

      const asset = await fetch(`http://127.0.0.1:${server.port}/asset.txt`);
      expect(asset.status).toBe(200);
      expect(await asset.text()).toBe("asset-body");

      const spa = await fetch(`http://127.0.0.1:${server.port}/chats/123`);
      expect(spa.status).toBe(200);
      expect(await spa.text()).toContain("peri");

      // 未知非 API 路径对所有方法都回退 index.html（静态路由保持 `app.all`）。
      const spaPost = await fetch(`http://127.0.0.1:${server.port}/chats/123`, {
        method: "POST",
      });
      expect(spaPost.status).toBe(200);
      expect(await spaPost.text()).toContain("peri");

      const spaOptions = await fetch(`http://127.0.0.1:${server.port}/chats/123`, {
        method: "OPTIONS",
      });
      expect(spaOptions.status).toBe(200);
      expect(await spaOptions.text()).toContain("peri");

      const apiRoot = await fetch(`http://127.0.0.1:${server.port}/api`);
      expect(apiRoot.status).toBe(404);
      expect(await apiRoot.text()).toBe("404 Not Found");

      const apiMissing = await fetch(`http://127.0.0.1:${server.port}/api/missing`);
      expect(apiMissing.status).toBe(404);
      expect(await apiMissing.text()).toBe("404 Not Found");

      const apiPost = await fetch(`http://127.0.0.1:${server.port}/api/unknown`, {
        method: "POST",
      });
      expect(apiPost.status).toBe(404);
      expect(await apiPost.text()).toBe("404 Not Found");
    } finally {
      server.stop(true);
      await registry.shutdown();
      rmSync(root, { recursive: true, force: true });
    }
  });
});
