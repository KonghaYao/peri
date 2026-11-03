import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { describe, expect, test } from "bun:test";
import type { ServerConfig } from "./config";
import type { InstanceBusinessSnapshot } from "./instance-metrics";
import {
  createMonitor,
  type MonitorDependencies,
  type MonitorRegistry,
  type SandboxSettingsSummary,
} from "./monitor";
import type { MonitorInstanceView } from "./registry";

const ID = "sbx_0123456789abcdef0123456789abcdef";
const NOW = Date.parse("2026-10-27T10:00:00.000Z");

const SETTINGS: SandboxSettingsSummary = {
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
};

function config(overrides: Partial<ServerConfig> = {}): ServerConfig {
  return {
    hostname: "127.0.0.1",
    port: 8787,
    machineId: "11111111-1111-4111-8111-111111111111",
    model: {
      provider: "openai",
      tiers: { fable: "gpt-fable", opus: "gpt-opus", sonnet: "gpt-sonnet", haiku: "gpt-haiku" },
      baseUrl: "https://api.example.com/v1",
      apiKey: "model-secret",
    },
    storage: { url: "https://store.example.com", authToken: "store-secret" },
    mcpServers: [],
    sandbox: { enabled: true, root: "/sandboxes", settingsPath: "/etc/peri/sandbox.json" },
    wasmModuleUrl: "file:///opt/peri/peri-wasm.js",
    staticRoot: "/tmp/peri-cf-dist",
    ...overrides,
  };
}

function registry(overrides: Partial<MonitorRegistry> = {}): MonitorRegistry & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    sandboxSnapshot() {
      calls.push("sandboxSnapshot");
      return { active: [], starting: 0 };
    },
    activeChatCount() {
      calls.push("activeChatCount");
      return 0;
    },
    runtimeInstances() {
      calls.push("runtimeInstances");
      return { instances: [], starting: 0, unconfirmed: false };
    },
    ...overrides,
  };
}

function business(overrides: Partial<InstanceBusinessSnapshot> = {}): InstanceBusinessSnapshot {
  return {
    running: false,
    lastActivityAtMs: NOW - 5_000,
    queueDepth: 0,
    pendingInteractions: 0,
    turns: {
      started: 0,
      completed: 0,
      cancelled: 0,
      error: 0,
      totalWallMs: 0,
      lastWallMs: null,
      activeWallMs: null,
    },
    tokens: null,
    context: null,
    tools: null,
    lastStopReason: null,
    ...overrides,
  };
}

function instance(overrides: Partial<MonitorInstanceView> = {}): MonitorInstanceView {
  return {
    chatId: "chat-a",
    sessionId: "session-a",
    generationId: "generation-a",
    state: "live",
    startedAtMs: NOW - 60_000,
    linearMemoryBytes: 16 * 1024 * 1024,
    business: business(),
    ...overrides,
  };
}

function deps(overrides: Partial<MonitorDependencies> = {}): MonitorDependencies {
  return {
    processMemory: () => ({ rss: 100, heapTotal: 80, heapUsed: 40, arrayBuffers: 64 }),
    uptimeSeconds: () => 3725.9,
    now: () => NOW,
    readSandboxSettings: async () => SETTINGS,
    countWorkspaces: async () => 2,
    ...overrides,
  };
}

/** 不注入 `readSandboxSettings`，走默认只读文件实现。 */
function fileDeps(): Partial<MonitorDependencies> {
  return {
    processMemory: () => ({ rss: 1, heapTotal: 1, heapUsed: 1, arrayBuffers: 0 }),
    uptimeSeconds: () => 1,
    now: () => NOW,
    countWorkspaces: async () => 0,
  };
}

function writeSettings(path: string, profiles: Record<string, unknown>): void {
  const dir = dirname(path);
  writeFileSync(
    path,
    JSON.stringify({
      sandboxesRoot: join(dir, "sandboxes"),
      periPath: join(dir, "peri"),
      periSha256: "a".repeat(64),
      profiles,
    }),
  );
}

const PROFILE = {
  allowedDomains: ["example.com"],
  runtimeReadPaths: ["/usr"],
  memoryMax: "2G",
  tasksMax: 4,
  cpuQuota: "50%",
};

describe("createMonitor", () => {
  test("聚合四段并去掉密钥、header 值与 URL 查询参数", async () => {
    const monitor = createMonitor({
      config: config({
        model: {
          provider: "openai",
          tiers: { fable: "gpt-fable", opus: "gpt-opus", sonnet: "gpt-sonnet", haiku: "gpt-haiku" },
          baseUrl: "https://api.example.com/v1?token=leak",
          apiKey: "model-secret",
        },
        mcpServers: [{
          name: "remote",
          url: "https://user:pass@mcp.example.com/mcp?key=leak",
          headers: [
            { name: "Authorization", value: "Bearer mcp-secret" },
            { name: "X-Test", value: "yes" },
          ],
        }],
      }),
      registry: registry({
        activeChatCount: () => 3,
        sandboxSnapshot: () => ({
          active: [{
            sandboxId: ID,
            state: "ready",
            refs: 2,
            startedAtMs: Date.parse("2026-10-27T09:00:00.000Z"),
            workspacePath: `/sandboxes/${ID}/workspace`,
            mcpUrl: "http://user:pass@127.0.0.1:9000/mcp?token=leak",
            mcpServerName: "workspace",
            lastError: { code: "start_failed", message: "x".repeat(400) },
          }],
          starting: 1,
        }),
      }),
      deps: deps(),
    });

    const snapshot = await monitor.snapshot();

    expect(snapshot.generatedAt).toBe("2026-10-27T10:00:00.000Z");
    expect(snapshot.uptimeSeconds).toBe(3725);
    expect(snapshot.llm).toEqual({
      provider: "openai",
      activeAlias: "sonnet",
      baseUrl: "https://api.example.com/v1",
      tiers: { fable: "gpt-fable", opus: "gpt-opus", sonnet: "gpt-sonnet", haiku: "gpt-haiku" },
    });
    expect(snapshot.mcp.remote).toEqual([{
      name: "remote",
      url: "https://mcp.example.com/mcp",
      headerNames: ["Authorization", "X-Test"],
    }]);
    expect(snapshot.mcp).not.toHaveProperty("sandbox");
    expect(snapshot.mcp).not.toHaveProperty("sandboxEnabled");
    expect(snapshot.wasm).toEqual({
      moduleUrl: "/opt/peri/peri-wasm.js",
      process: { rssBytes: 100, heapTotalBytes: 80, heapUsedBytes: 40, arrayBufferBytes: 64 },
      chatHandles: 3,
      startingInstances: 0,
      unconfirmed: false,
      truncated: false,
      linearMemoryTotalBytes: null,
      linearMemoryKnownInstances: 0,
      linearMemoryUnknownInstances: 0,
      instances: [],
    });
    expect(snapshot.wasm).not.toHaveProperty("activeChats");
    expect(snapshot.sandbox.settings).toEqual(SETTINGS);
    expect(snapshot.sandbox.workspacesOnDisk).toBe(2);
    expect(snapshot.sandbox.starting).toBe(1);
    expect(snapshot.sandbox.active[0]).toMatchObject({
      sandboxId: ID,
      state: "ready",
      refs: 2,
      startedAt: "2026-10-27T09:00:00.000Z",
      workspacePath: `/sandboxes/${ID}/workspace`,
      mcpUrl: "http://127.0.0.1:9000/mcp",
      mcpServerName: "workspace",
    });
    // 错误文案截断到 300 字符（末尾省略号）。
    expect(snapshot.sandbox.active[0]!.lastError!.message).toHaveLength(301);

    // 决策：WASM 段不给单实例预算，也不提供存储端点。
    expect(snapshot.wasm).not.toHaveProperty("limits");
    expect(snapshot).not.toHaveProperty("storage");

    const serialized = JSON.stringify(snapshot);
    for (const secret of ["model-secret", "store-secret", "mcp-secret", "leak", "user:pass"]) {
      expect(serialized).not.toContain(secret);
    }
  });

  test("sandbox 禁用时不读设置文件、不读工作区、不查询池", async () => {
    const calls: string[] = [];
    const monitor = createMonitor({
      config: config({
        sandbox: { enabled: false, root: "/sandboxes", settingsPath: "/etc/peri/sandbox.json" },
      }),
      registry: registry({
        sandboxSnapshot: () => {
          calls.push("sandboxSnapshot");
          return { active: [], starting: 0 };
        },
      }),
      deps: deps({
        readSandboxSettings: async () => {
          calls.push("readSandboxSettings");
          return SETTINGS;
        },
        countWorkspaces: async () => {
          calls.push("countWorkspaces");
          return 2;
        },
      }),
    });

    const snapshot = await monitor.snapshot();

    expect(calls).toEqual([]);
    expect(snapshot.mcp).toEqual({ remote: [] });
    expect(snapshot.mcp).not.toHaveProperty("sandboxEnabled");
    expect(snapshot.sandbox).toMatchObject({
      enabled: false,
      root: "/sandboxes",
      settings: {
        path: "/etc/peri/sandbox.json",
        source: "disabled",
        profileName: null,
        profile: null,
      },
      workspacesOnDisk: null,
      active: [],
      starting: 0,
    });
  });

  test("显式 settings 文件缺失时报告 error，不回退 defaults", async () => {
    const root = mkdtempSync(join(tmpdir(), "peri-cf-monitor-"));
    try {
      const settingsPath = join(root, "missing", "settings.json");
      const monitor = createMonitor({
        config: config({ sandbox: { enabled: true, root, settingsPath } }),
        registry: registry(),
        deps: fileDeps(),
      });
      const snapshot = await monitor.snapshot();
      expect(snapshot.sandbox.settings).toEqual({
        path: settingsPath,
        source: "error",
        profileName: null,
        profile: null,
        error: `Sandbox settings not found: ${settingsPath}`,
      });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("未配置 settingsPath 且默认文件缺失时报告 defaults 与运行时默认 profile", async () => {
    const root = mkdtempSync(join(tmpdir(), "peri-cf-monitor-"));
    const previous = process.env.PERI_SANDBOX_SETTINGS;
    try {
      const settingsPath = join(root, "settings.json");
      process.env.PERI_SANDBOX_SETTINGS = settingsPath;
      const monitor = createMonitor({
        config: config({ sandbox: { enabled: true, root } }),
        registry: registry(),
        deps: fileDeps(),
      });
      const snapshot = await monitor.snapshot();
      expect(snapshot.sandbox.settings).toMatchObject({
        path: settingsPath,
        source: "defaults",
        profileName: "workspace-default",
        profile: { memoryMax: "1G", tasksMax: 128, cpuQuota: "100%" },
      });
      expect(snapshot.sandbox.settings.profile?.allowedDomainsTotal).toBeGreaterThan(0);
    } finally {
      if (previous === undefined) delete process.env.PERI_SANDBOX_SETTINGS;
      else process.env.PERI_SANDBOX_SETTINGS = previous;
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("无 defaultProfile 时按运行时规则回退到首个 profile", async () => {
    const root = mkdtempSync(join(tmpdir(), "peri-cf-monitor-"));
    try {
      const settingsPath = join(root, "settings.json");
      writeSettings(settingsPath, { beta: PROFILE, alpha: PROFILE });
      const monitor = createMonitor({
        config: config({ sandbox: { enabled: true, root, settingsPath } }),
        registry: registry(),
        deps: fileDeps(),
      });
      const snapshot = await monitor.snapshot();
      expect(snapshot.sandbox.settings).toEqual({
        path: settingsPath,
        source: "file",
        profileName: "beta",
        profile: {
          memoryMax: "2G",
          tasksMax: 4,
          cpuQuota: "50%",
          allowedDomains: ["example.com"],
          allowedDomainsTotal: 1,
          runtimeReadPathsCount: 1,
        },
      });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("请求的 profile 不存在时报告与运行时一致的错误", async () => {
    const root = mkdtempSync(join(tmpdir(), "peri-cf-monitor-"));
    try {
      const settingsPath = join(root, "settings.json");
      writeSettings(settingsPath, { beta: PROFILE });
      const monitor = createMonitor({
        config: config({ sandbox: { enabled: true, root, settingsPath, profileName: "missing" } }),
        registry: registry(),
        deps: fileDeps(),
      });
      const snapshot = await monitor.snapshot();
      expect(snapshot.sandbox.settings).toEqual({
        path: settingsPath,
        source: "error",
        profileName: "missing",
        profile: null,
        error: "Unknown sandbox profile: missing.",
      });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("设置读取异常收敛为 source=error，不中断其余段落", async () => {
    const monitor = createMonitor({
      config: config(),
      registry: registry({ activeChatCount: () => 1 }),
      deps: deps({
        readSandboxSettings: async () => {
          throw new Error("settings unreadable");
        },
      }),
    });

    const snapshot = await monitor.snapshot();
    expect(snapshot.sandbox.settings).toMatchObject({
      path: "/etc/peri/sandbox.json",
      source: "error",
      profileName: null,
      profile: null,
      error: "settings unreadable",
    });
    expect(snapshot.wasm.chatHandles).toBe(1);
  });

  test("工作区计数失败返回 null + 原因，且失败不缓存", async () => {
    let reads = 0;
    const monitor = createMonitor({
      config: config(),
      registry: registry(),
      deps: deps({
        countWorkspaces: async () => {
          reads += 1;
          if (reads === 1) throw new Error("disk down");
          return 4;
        },
      }),
    });

    const failed = await monitor.snapshot();
    expect(failed.sandbox.workspacesOnDisk).toBeNull();
    expect(failed.sandbox.workspacesError).toBe("disk down");

    const recovered = await monitor.snapshot();
    expect(recovered.sandbox.workspacesOnDisk).toBe(4);
    expect(recovered.sandbox).not.toHaveProperty("workspacesError");
    expect(reads).toBe(2);
  });

  test("实例段映射身份、线性内存合计与业务计数", async () => {
    const monitor = createMonitor({
      config: config(),
      registry: registry({
        runtimeInstances: () => ({
          instances: [
            instance({
              chatId: "chat-a",
              sessionId: "session-a",
              generationId: "gen-a",
              state: "live",
              startedAtMs: NOW - 60_000,
              linearMemoryBytes: 16 * 1024 * 1024,
              business: business({
                running: true,
                lastActivityAtMs: NOW - 5_000,
                queueDepth: 2,
                pendingInteractions: 1,
                turns: {
                  started: 4,
                  completed: 3,
                  cancelled: 0,
                  error: 1,
                  totalWallMs: 120_000,
                  lastWallMs: 30_000,
                  activeWallMs: 5_000,
                },
                tokens: {
                  llmCalls: 12,
                  inputTokens: 34_567,
                  outputTokens: 1_234,
                  auxiliaryCalls: 2,
                  partial: false,
                },
                context: { used: 12_345, size: 200_000 },
                tools: { started: 5, completed: 4, failed: 1, cancelled: 0 },
                lastStopReason: "end_turn",
              }),
            }),
            instance({
              chatId: "chat-b",
              sessionId: null,
              generationId: "gen-b",
              state: "close-unconfirmed",
              startedAtMs: NOW - 10_000,
              linearMemoryBytes: null,
              business: null,
            }),
          ],
          starting: 1,
          unconfirmed: true,
        }),
      }),
      deps: deps(),
    });

    const snapshot = await monitor.snapshot();

    expect(snapshot.wasm.chatHandles).toBe(0);
    expect(snapshot.wasm.startingInstances).toBe(1);
    expect(snapshot.wasm.unconfirmed).toBe(true);
    expect(snapshot.wasm.linearMemoryTotalBytes).toBe(16 * 1024 * 1024);
    expect(snapshot.wasm.linearMemoryKnownInstances).toBe(1);
    expect(snapshot.wasm.linearMemoryUnknownInstances).toBe(1);
    expect(snapshot.wasm.instances[0]).toEqual({
      chatId: "chat-a",
      sessionId: "session-a",
      generationId: "gen-a",
      state: "live",
      startedAt: "2026-10-27T09:59:00.000Z",
      uptimeSeconds: 60,
      linearMemoryBytes: 16 * 1024 * 1024,
      running: true,
      lastActivityAt: "2026-10-27T09:59:55.000Z",
      queueDepth: 2,
      pendingInteractions: 1,
      turns: {
        started: 4,
        completed: 3,
        cancelled: 0,
        error: 1,
        totalWallMs: 120_000,
        lastWallMs: 30_000,
        activeWallMs: 5_000,
      },
      tokens: {
        llmCalls: 12,
        inputTokens: 34_567,
        outputTokens: 1_234,
        auxiliaryCalls: 2,
        partial: false,
      },
      context: { used: 12_345, size: 200_000 },
      tools: { started: 5, completed: 4, failed: 1, cancelled: 0 },
      lastStopReason: "end_turn",
    });
    expect(snapshot.wasm.instances[1]).toMatchObject({
      chatId: "chat-b",
      state: "close-unconfirmed",
      linearMemoryBytes: null,
      statsError: true,
      turns: { started: 0 },
      tokens: null,
      context: null,
      tools: null,
    });
  });

  test("实例段失败时只标记 instancesError，进程指标仍返回", async () => {
    const monitor = createMonitor({
      config: config(),
      registry: registry({
        runtimeInstances: () => { throw new Error("instance snapshot failed"); },
      }),
      deps: deps(),
    });

    const snapshot = await monitor.snapshot();
    expect(snapshot.wasm.instancesError).toBe("instance snapshot failed");
    expect(snapshot.wasm.instances).toEqual([]);
    expect(snapshot.wasm.process.rssBytes).toBe(100);
    expect(snapshot.wasm.linearMemoryTotalBytes).toBeNull();
  });

  test("无实例时不伪造内存合计；超过上限时截断并标记", async () => {
    const empty = createMonitor({ config: config(), registry: registry(), deps: deps() });
    const emptySnapshot = await empty.snapshot();
    expect(emptySnapshot.wasm.instances).toEqual([]);
    expect(emptySnapshot.wasm.linearMemoryTotalBytes).toBeNull();
    expect(emptySnapshot.wasm.truncated).toBe(false);

    const many = Array.from({ length: 201 }, (_, index) =>
      instance({ chatId: `chat-${index}`, linearMemoryBytes: 65_536 }));
    const truncated = createMonitor({
      config: config(),
      registry: registry({
        runtimeInstances: () => ({ instances: many, starting: 0, unconfirmed: false }),
      }),
      deps: deps(),
    });
    const snapshot = await truncated.snapshot();
    expect(snapshot.wasm.instances).toHaveLength(200);
    expect(snapshot.wasm.truncated).toBe(true);
    expect(snapshot.wasm.linearMemoryTotalBytes).toBe(200 * 65_536);
  });

  test("实例 wire DTO 只含白名单字段，不携带内容", async () => {
    const monitor = createMonitor({
      config: config(),
      registry: registry({
        runtimeInstances: () => ({
          instances: [instance({ chatId: "chat-secret" })],
          starting: 0,
          unconfirmed: false,
        }),
      }),
      deps: deps(),
    });
    const snapshot = await monitor.snapshot();
    const wire = snapshot.wasm.instances[0]!;
    expect(Object.keys(wire).sort()).toEqual([
      "chatId",
      "context",
      "generationId",
      "lastActivityAt",
      "lastStopReason",
      "linearMemoryBytes",
      "pendingInteractions",
      "queueDepth",
      "running",
      "sessionId",
      "startedAt",
      "state",
      "tokens",
      "tools",
      "turns",
      "uptimeSeconds",
    ]);
    for (const forbidden of ["title", "arguments", "result", "message", "content", "error", "prompt"]) {
      expect(wire).not.toHaveProperty(forbidden);
    }
  });

  test("settings 缓存 30s、工作区缓存 10s，按注入时钟失效", async () => {
    let nowMs = NOW;
    let settingsReads = 0;
    let workspaceReads = 0;
    const monitor = createMonitor({
      config: config(),
      registry: registry(),
      deps: deps({
        now: () => nowMs,
        readSandboxSettings: async () => {
          settingsReads += 1;
          return SETTINGS;
        },
        countWorkspaces: async () => {
          workspaceReads += 1;
          return 2;
        },
      }),
    });

    await monitor.snapshot();
    await monitor.snapshot();
    expect(settingsReads).toBe(1);
    expect(workspaceReads).toBe(1);

    nowMs += 10_001;
    await monitor.snapshot();
    expect(settingsReads).toBe(1);
    expect(workspaceReads).toBe(2);

    nowMs += 20_000;
    await monitor.snapshot();
    expect(settingsReads).toBe(2);
  });
});
