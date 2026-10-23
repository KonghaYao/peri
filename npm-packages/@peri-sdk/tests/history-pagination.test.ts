import { describe, expect, test } from "bun:test";
import { HistoryPageUnavailableError } from "../src/agent/session";
import { MemoryKV } from "../src/kv/memory-kv";
import { ManagedAgents } from "../src/managed/managed-agents";
import { Sandbox } from "../src/sandbox/sandbox";
import type { JsonRpcNotification, Transport } from "../src/transport/types";

// 旧私有分页（`peri.historyPagination` / `session/load + cursor` / `loadGeneration`）
// 已从服务端删除。SDK 不再声明、不再发送这些字段；`loadEarlierHistory` 显式失败，
// 不会静默回退到旧协议。

const SESSION = "s1";

class RecordingTransport implements Transport {
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  private readonly listeners = new Set<(notification: JsonRpcNotification) => void>();

  async request<T>(method: string, params?: unknown): Promise<T> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as T;
    if (method === "session/new") return { sessionId: SESSION } as T;
    if (method === "session/load") {
      return { sessionId: SESSION, modes: {}, configOptions: [] } as T;
    }
    if (method === "session/input/snapshot") return { generation: "generation-1" } as T;
    if (method === "session/close") return {} as T;
    throw new Error(`Unexpected ACP request: ${method}`);
  }

  async sendRequest<T>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
    return { response: this.request<T>(method, params) };
  }
  async notify(): Promise<void> {}
  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  async *events(): AsyncIterable<JsonRpcNotification> {}
  setRequestHandler(): void {}
  async close(): Promise<void> {}
}

const storage = {
  getSessions: async () => [],
  getSession: async (id: string) => ({
    id, cwd: "/tmp/workspace", title: null, messageCount: 0, createdAt: "now", updatedAt: "now",
  }),
};

function makeAgent(transport: RecordingTransport, id: string) {
  return new ManagedAgents({ kv: new MemoryKV() }).createAgent({
    path: "/tmp/workspace",
    id,
    sandbox: new Sandbox({ id: `${id}-workspace`, storage, transportFactory: () => transport }),
  });
}

describe("retired history pagination", () => {
  test("initialize does not declare peri.historyPagination", async () => {
    const transport = new RecordingTransport();
    const agent = makeAgent(transport, "no-pagination-agent");
    await agent.session.start(SESSION);

    const initialize = transport.calls.find((call) => call.method === "initialize")?.params as {
      clientCapabilities?: { _meta?: Record<string, unknown> };
    };
    expect(initialize.clientCapabilities?._meta?.["peri.historyPagination"]).toBeUndefined();
    await agent.close();
  });

  test("session/load carries no legacy pagination or generation fields", async () => {
    const transport = new RecordingTransport();
    const agent = makeAgent(transport, "clean-load-agent");
    await agent.session.start(SESSION);

    const load = transport.calls.find((call) => call.method === "session/load")?.params as Record<string, unknown>;
    expect(load.sessionId).toBe(SESSION);
    expect(load.cursor).toBeUndefined();
    expect(load.limit).toBeUndefined();
    expect(load.loadGeneration).toBeUndefined();
    expect(load.history).toBeUndefined();
    await agent.close();
  });

  test("loadEarlierHistory fails explicitly instead of sending the old protocol", async () => {
    const transport = new RecordingTransport();
    const agent = makeAgent(transport, "unsupported-agent");
    await agent.session.start(SESSION);

    expect(agent.session.canLoadEarlierHistory).toBe(false);
    await expect(agent.session.loadEarlierHistory()).rejects.toBeInstanceOf(HistoryPageUnavailableError);
    await expect(agent.session.loadEarlierHistory()).rejects.toMatchObject({ reason: "not_negotiated" });
    expect(transport.calls.filter((call) => call.method === "session/load")).toHaveLength(1);
    await agent.close();
  });
});
