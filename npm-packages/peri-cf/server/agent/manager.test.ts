import { describe, expect, test } from "bun:test";
import type { SessionStorage, SessionSummary } from "@peri-code/sdk/portable";
import type { WasmAcpTransport } from "@peri-code/sdk/wasm";
import { SESSION_CWD, WORKSPACE_CWD, type ServerConfig } from "../config";
import { ChatManager } from "./manager";
import { TursoChatSessionStore } from "../store";
import { SYSTEM_INSTRUCTIONS } from "../system-instructions";
import type { AgentEnvironment, StartHost } from "../../core/agent";

const config: ServerConfig = {
  hostname: "127.0.0.1",
  port: 8787,
  machineId: "11111111-1111-4111-8111-111111111111",
  model: { provider: "openai", id: "gpt-test", baseUrl: "https://api.example.com/v1", apiKey: "test-key" },
  storage: { url: "https://store.example.com", authToken: "test-token" },
  mcpServers: [{
    name: "remote", url: "https://mcp.example.com/mcp",
    headers: [{ name: "Authorization", value: "Bearer test" }],
  }],
  wasmModuleUrl: "file:///tmp/peri-wasm.js",
  staticRoot: "/tmp/dist/client",
};

function summary(id: string, overrides: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id, title: null, cwd: SESSION_CWD, messageCount: 2,
    createdAt: "2026-10-20T12:00:00Z", updatedAt: "2026-10-20T12:00:00Z",
    ...overrides,
  };
}

interface RecordingStorage {
  storage: Pick<SessionStorage, "getSessions" | "getSession">;
  sessionCalls: string[];
  getIds: string[];
}

function recordingStorage(sessions: readonly SessionSummary[]): RecordingStorage {
  const sessionCalls: string[] = [];
  const getIds: string[] = [];
  return {
    sessionCalls,
    getIds,
    storage: {
      async getSessions(cwd) {
        sessionCalls.push(cwd);
        return sessions.filter((session) => session.cwd === cwd);
      },
      async getSession(id) {
        getIds.push(id);
        return sessions.find((session) => session.id === id) ?? null;
      },
    },
  };
}

const unusedHost: StartHost = async () => { throw new Error("host is not started in manager tests"); };

const environment: AgentEnvironment = {
  cwd: SESSION_CWD,
  instructions: "test instructions",
  initialize: async () => {},
  openMcp: () => ({
    servers: [],
    handleMessage: () => { throw new Error("host is not started in manager tests"); },
    close: async () => {},
  }),
  logError: () => {},
};

/** 满足 `SessionCreator` 的最小传输，并记录请求参数。 */
function capturingStartHost(
  sessionId: string, requests: { method: string; params: unknown }[],
): StartHost {
  return async () => ({
    setRequestHandler() {},
    request: async (method: string, params: unknown) => {
      requests.push({ method, params });
      if (method === "initialize") return { protocolVersion: 1 };
      return method === "session/new"
        ? { sessionId }
        : { sessionId, title: (params as { title: string }).title };
    },
    close: async () => {},
  }) as unknown as WasmAcpTransport;
}

describe("ChatManager 注入关系", () => {
  test("注入的 store 原样供 ChatService.list 使用", async () => {
    const recording = recordingStorage([
      summary("a", { updatedAt: "2026-10-20T12:00:00Z" }),
      summary("b", { cwd: WORKSPACE_CWD, updatedAt: "2026-10-20T13:00:00Z" }),
    ]);
    const manager = new ChatManager(config, {
      store: new TursoChatSessionStore(recording.storage),
      startHost: unusedHost,
      createEnvironment: () => ({ ...environment }),
    });

    const listed = await manager.list();

    expect(listed.map((chat) => chat.id)).toEqual(["b", "a"]);
    expect(recording.sessionCalls).toEqual([WORKSPACE_CWD, SESSION_CWD]);
  });

  test("同一个 store 既用于 service 也进入 PeriInstance 的会话校验", async () => {
    const recording = recordingStorage([]);
    const manager = new ChatManager(config, {
      store: new TursoChatSessionStore(recording.storage),
      startHost: unusedHost,
      createEnvironment: () => ({ ...environment }),
    });

    await expect(manager.read("a")).rejects.toThrow("Chat not found");

    expect(recording.getIds).toEqual(["a"]);
  });

  test("两个 PeriInstance 获得不同的环境对象与各自的 chatId", async () => {
    const recording = recordingStorage([]);
    const environmentIds: (string | undefined)[] = [];
    const environments: AgentEnvironment[] = [];
    const manager = new ChatManager(config, {
      store: new TursoChatSessionStore(recording.storage),
      startHost: unusedHost,
      createEnvironment: (chatId) => {
        environmentIds.push(chatId);
        const created = { ...environment };
        environments.push(created);
        return created;
      },
    });

    await Promise.allSettled([manager.read("a"), manager.read("b")]);

    expect(environmentIds).toEqual(["a", "b"]);
    expect(environments).toHaveLength(2);
    expect(environments[0]).not.toBe(environments[1]);
  });

  test("默认环境按配置声明远程 MCP 与系统指令", async () => {
    const createdId = "11111111-1111-4111-8111-111111111111";
    const recording = recordingStorage([summary(createdId, { title: "新会话" })]);
    const requests: { method: string; params: unknown }[] = [];
    const manager = new ChatManager(config, {
      store: new TursoChatSessionStore(recording.storage),
      startHost: capturingStartHost(createdId, requests),
    });

    const created = await manager.create("新会话");

    expect(created).toEqual({
      id: createdId, title: "新会话", updatedAt: "2026-10-20T12:00:00Z",
    });
    const newSession = requests.find((request) => request.method === "session/new");
    expect(newSession?.params).toMatchObject({
      cwd: SESSION_CWD,
      mcpServers: [{
        type: "http", name: "remote", url: "https://mcp.example.com/mcp",
        headers: [{ name: "Authorization", value: "Bearer test" }],
      }],
      _meta: { "peri.instructions": SYSTEM_INSTRUCTIONS },
    });
  });

  test("shutdown 委托给 service 且可重复调用", async () => {
    const manager = new ChatManager(config, {
      store: new TursoChatSessionStore(recordingStorage([]).storage),
      startHost: unusedHost,
      createEnvironment: () => ({ ...environment }),
    });

    await expect(manager.shutdown()).resolves.toBeUndefined();
    await expect(manager.shutdown()).resolves.toBeUndefined();
  });
});
