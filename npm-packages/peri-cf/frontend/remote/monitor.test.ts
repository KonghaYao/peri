import { describe, expect, test } from "bun:test";
import { MonitorUnavailableError, createHttpMonitorRepository } from "./monitor";
import type { MonitorSnapshot } from "./contracts";

const snapshot: MonitorSnapshot = {
  ok: true,
  generatedAt: "2026-10-27T10:00:00.000Z",
  uptimeSeconds: 60,
  llm: {
    provider: "openai",
    activeAlias: "sonnet",
    baseUrl: "https://api.example.com/v1",
    tiers: { fable: "gpt-fable", opus: "gpt-opus", sonnet: "gpt-sonnet", haiku: "gpt-haiku" },
  },
  mcp: {
    remote: [{ name: "remote", url: "https://mcp.example.com/mcp", headerNames: ["Authorization"] }],
  },
  wasm: {
    moduleUrl: "/opt/peri/peri-wasm.js",
    process: { rssBytes: 1, heapTotalBytes: 2, heapUsedBytes: 3, arrayBufferBytes: 4 },
    chatHandles: 1,
    startingInstances: 0,
    unconfirmed: false,
    truncated: false,
    linearMemoryTotalBytes: 65_536,
    linearMemoryKnownInstances: 1,
    linearMemoryUnknownInstances: 0,
    instances: [{
      chatId: "chat-1",
      sessionId: "session-1",
      generationId: "generation-1",
      state: "live",
      startedAt: "2026-10-27T09:59:00.000Z",
      uptimeSeconds: 60,
      linearMemoryBytes: 65_536,
      running: false,
      lastActivityAt: "2026-10-27T09:59:55.000Z",
      queueDepth: 0,
      pendingInteractions: 0,
      turns: {
        started: 1,
        completed: 1,
        cancelled: 0,
        error: 0,
        totalWallMs: 1_000,
        lastWallMs: 1_000,
        activeWallMs: null,
      },
      tokens: null,
      context: null,
      tools: null,
      lastStopReason: "end_turn",
    }],
  },
  sandbox: {
    enabled: true,
    root: "/sandboxes",
    settings: {
      path: "/etc/peri/sandbox.json",
      source: "file",
      profileName: "default",
      profile: {
        memoryMax: "1G",
        tasksMax: 128,
        cpuQuota: "100%",
        allowedDomains: ["github.com"],
        allowedDomainsTotal: 1,
        runtimeReadPathsCount: 3,
      },
    },
    workspacesOnDisk: 2,
    active: [],
    starting: 0,
  },
};

function respond(body: unknown, status = 200): typeof fetch {
  return (async () => new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })) as unknown as typeof fetch;
}

describe("monitor repository", () => {
  test("读取并校验四段快照", async () => {
    const repository = createHttpMonitorRepository("http://localhost", respond(snapshot));
    expect(await repository.read()).toEqual(snapshot);
  });

  test("契约不匹配时保留失败，不伪造数据", async () => {
    const invalid: Record<string, unknown> = { ...snapshot };
    delete invalid.sandbox;
    const repository = createHttpMonitorRepository("http://localhost", respond(invalid));
    const failure = await repository.read().then(() => undefined, (error: unknown) => error);
    expect(failure).toBeInstanceOf(MonitorUnavailableError);
    expect((failure as Error).message).toBe("监控数据不可用（HTTP 200）。");
    expect((failure as MonitorUnavailableError).status).toBe(200);
  });

  test("503 错误封套的 message 透传给面板", async () => {
    const repository = createHttpMonitorRepository("http://localhost", respond({
      ok: false,
      code: "monitor_unavailable",
      message: "Monitor snapshot is unavailable",
    }, 503));
    const failure = await repository.read().then(() => undefined, (error: unknown) => error);
    expect(failure).toBeInstanceOf(MonitorUnavailableError);
    expect((failure as Error).message).toBe("Monitor snapshot is unavailable");
    expect((failure as MonitorUnavailableError).status).toBe(503);
  });

  test("网络失败映射为 status 0", async () => {
    const repository = createHttpMonitorRepository("http://localhost", (async () => {
      throw new Error("connection refused");
    }) as unknown as typeof fetch);
    const failure = await repository.read().then(() => undefined, (error: unknown) => error);
    expect(failure).toBeInstanceOf(MonitorUnavailableError);
    expect((failure as Error).message).toBe("connection refused");
    expect((failure as MonitorUnavailableError).status).toBe(0);
  });
});
