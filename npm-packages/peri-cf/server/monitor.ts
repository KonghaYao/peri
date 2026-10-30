import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defaultSettingsPath, resolveSandboxSettings, type SandboxProfile } from "peri-sandbox";
import {
  publicOrigin,
  type ModelProvider,
  type ModelTier,
  type ServerConfig,
} from "./config";
import { DEFAULT_MODEL_ALIAS } from "./compose";
import type { MonitorInstanceView, MonitorRuntimeView } from "./registry";
import {
  listSandboxWorkspaces,
  type SandboxPoolSnapshot,
  type SandboxRuntimeSnapshot,
} from "./sandbox";

/**
 * 只读服务端监控快照。
 *
 * 数据只来自「已有配置 + 内存事实 + 只读文件读取」：读取不会启动 sandbox 或 WASM
 * 运行后端，也不会执行 Agent。WASM 线性内存可从 SDK 精确读取（当前容量，不是活跃堆）；
 * JS/Bun 堆与 CPU 无法按实例拆分，因此只给进程级内存作上下文，不按实例均摊。
 */

const SETTINGS_CACHE_TTL_MS = 30_000;
const WORKSPACE_CACHE_TTL_MS = 10_000;
const MAX_ERROR_MESSAGE_LENGTH = 300;
const MAX_DOMAINS = 100;
/** 单次快照的实例上限；超出时只映射前 N 条并标记 `truncated`。 */
const MAX_MONITOR_INSTANCES = 200;

export type SandboxStateLabel = SandboxRuntimeSnapshot["state"];

export interface SandboxProfileSummary {
  memoryMax: string;
  tasksMax: number;
  cpuQuota: string;
  /** 最多 100 条；完整数量见 `allowedDomainsTotal`。 */
  allowedDomains: string[];
  allowedDomainsTotal: number;
  runtimeReadPathsCount: number;
}

export interface SandboxSettingsSummary {
  path: string;
  source: "file" | "defaults" | "disabled" | "error";
  profileName: string | null;
  profile: SandboxProfileSummary | null;
  error?: string;
}

/** WASM 实例的 wire DTO：只含数值、标识与已知状态枚举。 */
export interface WasmInstanceSnapshot {
  chatId: string;
  sessionId: string | null;
  generationId: string;
  state: MonitorInstanceView["state"];
  startedAt: string;
  uptimeSeconds: number;
  /** 线性内存当前容量；宿主未暴露时为 null，不用 0 冒充。 */
  linearMemoryBytes: number | null;
  running: boolean;
  lastActivityAt: string | null;
  queueDepth: number;
  pendingInteractions: number;
  turns: {
    started: number;
    completed: number;
    cancelled: number;
    error: number;
    totalWallMs: number;
    lastWallMs: number | null;
    activeWallMs: number | null;
  };
  tokens: {
    llmCalls: number;
    inputTokens: number | null;
    outputTokens: number | null;
    auxiliaryCalls: number;
    partial: boolean;
  } | null;
  context: { used: number; size: number } | null;
  tools: {
    started: number;
    completed: number;
    failed: number;
    cancelled: number;
  } | null;
  lastStopReason: string | null;
  /** 业务计数不可用（订阅失败）；此时 turns 为 0 值，其余可空字段为 null。 */
  statsError?: true;
}

export interface MonitorSnapshot {
  generatedAt: string;
  uptimeSeconds: number;
  llm: {
    provider: ModelProvider;
    activeAlias: ModelTier;
    /** 去敏（protocol + host + path）。 */
    baseUrl: string;
    tiers: Record<ModelTier, string>;
  };
  mcp: {
    remote: Array<{ name: string; url: string; headerNames: string[] }>;
  };
  wasm: {
    moduleUrl: string;
    process: {
      rssBytes: number;
      heapTotalBytes: number;
      heapUsedBytes: number;
      /** 进程合计，包含全部 WASM 线性内存；不能拆分到单个实例。 */
      arrayBufferBytes: number;
    };
    /** 已创建、未关闭的 Chat 句柄；运行后端数量的上限。 */
    chatHandles: number;
    /** 尚未取得租约的启动任务数（无 chat 归属，不进入 `instances`）。 */
    startingInstances: number;
    /** 曾有实例关闭未确认；在进程关闭前保持。 */
    unconfirmed: boolean;
    /** 实例段读取失败的原因；此时进程级指标仍有效。 */
    instancesError?: string;
    /** 实例数超过上限，`instances` 只含前 N 条。 */
    truncated: boolean;
    /** 已知实例的线性内存合计；全部不可读或无实例时为 null。 */
    linearMemoryTotalBytes: number | null;
    linearMemoryKnownInstances: number;
    linearMemoryUnknownInstances: number;
    instances: WasmInstanceSnapshot[];
  };
  sandbox: {
    enabled: boolean;
    root: string;
    settings: SandboxSettingsSummary;
    workspacesOnDisk: number | null;
    workspacesError?: string;
    active: Array<{
      sandboxId: string;
      state: SandboxStateLabel;
      refs: number;
      startedAt: string;
      workspacePath: string;
      mcpUrl: string | null;
      mcpServerName: string | null;
      lastError: { code: string; message: string } | null;
    }>;
    starting: number;
  };
}

export interface MonitorSource {
  snapshot(): Promise<MonitorSnapshot>;
}

/** 结构化注册表读面；不依赖具体 `ChatRegistry` 装配。 */
export interface MonitorRegistry {
  sandboxSnapshot(): SandboxPoolSnapshot | undefined;
  activeChatCount(): number;
  runtimeInstances(nowMs: number): MonitorRuntimeView;
}

export interface MonitorDependencies {
  processMemory(): {
    rss: number;
    heapTotal: number;
    heapUsed: number;
    arrayBuffers: number;
  };
  uptimeSeconds(): number;
  now(): number;
  readSandboxSettings(): Promise<SandboxSettingsSummary>;
  countWorkspaces(): Promise<number>;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function truncate(value: string, limit = MAX_ERROR_MESSAGE_LENGTH): string {
  return value.length > limit ? `${value.slice(0, limit)}…` : value;
}

function isoFromMs(ms: number | null): string | null {
  return ms === null ? null : new Date(ms).toISOString();
}

/** 单实例 wire DTO；业务计数缺失时显式标记 `statsError`，不用 0 冒充 token/工具。 */
function mapWasmInstance(instance: MonitorInstanceView, generatedAtMs: number): WasmInstanceSnapshot {
  const business = instance.business;
  return {
    chatId: instance.chatId,
    sessionId: instance.sessionId,
    generationId: instance.generationId,
    state: instance.state,
    startedAt: new Date(instance.startedAtMs).toISOString(),
    uptimeSeconds: Math.max(0, Math.floor((generatedAtMs - instance.startedAtMs) / 1000)),
    linearMemoryBytes: instance.linearMemoryBytes,
    running: business?.running ?? false,
    lastActivityAt: isoFromMs(business?.lastActivityAtMs ?? null),
    queueDepth: business?.queueDepth ?? 0,
    pendingInteractions: business?.pendingInteractions ?? 0,
    turns: business?.turns ?? {
      started: 0,
      completed: 0,
      cancelled: 0,
      error: 0,
      totalWallMs: 0,
      lastWallMs: null,
      activeWallMs: null,
    },
    tokens: business?.tokens ?? null,
    context: business?.context ?? null,
    tools: business?.tools ?? null,
    lastStopReason: business?.lastStopReason ?? null,
    ...(business === null ? { statsError: true as const } : {}),
  };
}

function publicUrl(raw: string | null): string | null {
  if (raw === null) return null;
  return publicOrigin(raw) || null;
}

/** `file:` 展示本地路径，http(s) 只展示 origin + path。 */
function displayModuleUrl(raw: string): string {
  if (raw.startsWith("file:")) {
    try {
      return fileURLToPath(raw);
    } catch {
      return raw;
    }
  }
  return publicOrigin(raw) || raw;
}

function summarizeProfile(profile: SandboxProfile): SandboxProfileSummary {
  const domains = profile.allowedDomains;
  return {
    memoryMax: profile.memoryMax,
    tasksMax: profile.tasksMax,
    cpuQuota: profile.cpuQuota,
    allowedDomains: domains.slice(0, MAX_DOMAINS),
    allowedDomainsTotal: domains.length,
    runtimeReadPathsCount: profile.runtimeReadPaths.length,
  };
}

/** 单飞 + TTL 缓存；只缓存成功结果，失败可立即重试。 */
function cached<T>(ttlMs: number, now: () => number, load: () => Promise<T>): () => Promise<T> {
  let value: T | undefined;
  let loadedAt = 0;
  let pending: Promise<T> | undefined;
  return () => {
    if (value !== undefined && now() - loadedAt < ttlMs) return Promise.resolve(value);
    if (pending) return pending;
    const request = load().then(
      (result) => {
        value = result;
        loadedAt = now();
        pending = undefined;
        return result;
      },
      (error: unknown) => {
        pending = undefined;
        throw error;
      },
    );
    pending = request;
    return request;
  };
}

function settingsReader(config: ServerConfig, path: string): () => Promise<SandboxSettingsSummary> {
  return async () => {
    try {
      // 与运行时共用文件加载与 profile 选择规则；缺省文件缺失时走内置默认值，
      // 但不会为了监控轮询去哈希 Peri 二进制。
      const resolved = resolveSandboxSettings({
        settingsPath: config.sandbox.settingsPath,
        profileName: config.sandbox.profileName,
      });
      return {
        path: resolved.path,
        source: resolved.source,
        profileName: resolved.profileName,
        profile: summarizeProfile(resolved.profile),
      };
    } catch (error) {
      return {
        path,
        source: "error",
        profileName: config.sandbox.profileName ?? null,
        profile: null,
        error: truncate(errorMessage(error)),
      };
    }
  };
}

export function createMonitor(input: {
  config: ServerConfig;
  registry: MonitorRegistry;
  deps?: Partial<MonitorDependencies>;
}): MonitorSource {
  const { config, registry } = input;
  const settingsPath = resolve(config.sandbox.settingsPath ?? defaultSettingsPath());
  const deps: MonitorDependencies = {
    processMemory: () => process.memoryUsage(),
    uptimeSeconds: () => process.uptime(),
    now: () => Date.now(),
    readSandboxSettings: settingsReader(config, settingsPath),
    countWorkspaces: async () => (await listSandboxWorkspaces(config.sandbox.root)).length,
    ...input.deps,
  };

  const readSettings = cached(SETTINGS_CACHE_TTL_MS, deps.now, deps.readSandboxSettings);
  const readWorkspaceCount = cached(WORKSPACE_CACHE_TTL_MS, deps.now, deps.countWorkspaces);

  async function settings(): Promise<SandboxSettingsSummary> {
    if (!config.sandbox.enabled) {
      return {
        path: settingsPath,
        source: "disabled",
        profileName: config.sandbox.profileName ?? null,
        profile: null,
      };
    }
    try {
      return await readSettings();
    } catch (error) {
      return {
        path: settingsPath,
        source: "error",
        profileName: config.sandbox.profileName ?? null,
        profile: null,
        error: truncate(errorMessage(error)),
      };
    }
  }

  async function workspaces(): Promise<{ count: number | null; error?: string }> {
    if (!config.sandbox.enabled) return { count: null };
    try {
      return { count: await readWorkspaceCount() };
    } catch (error) {
      return { count: null, error: truncate(errorMessage(error)) };
    }
  }

  return {
    async snapshot(): Promise<MonitorSnapshot> {
      const generatedAtMs = deps.now();
      const pool = config.sandbox.enabled ? registry.sandboxSnapshot() : undefined;
      const active = pool?.active ?? [];
      const memory = deps.processMemory();
      const [sandboxSettings, disk] = await Promise.all([settings(), workspaces()]);

      // 实例段失败只降级本段；进程级指标与 sandbox 段继续返回 200。
      let runtime: MonitorRuntimeView = { instances: [], starting: 0, unconfirmed: false };
      let instancesError: string | undefined;
      try {
        runtime = registry.runtimeInstances(generatedAtMs);
      } catch (error) {
        instancesError = truncate(errorMessage(error));
      }
      const truncated = runtime.instances.length > MAX_MONITOR_INSTANCES;
      const shown = truncated
        ? runtime.instances.slice(0, MAX_MONITOR_INSTANCES)
        : runtime.instances;
      let linearMemoryTotalBytes = 0;
      let linearMemoryKnownInstances = 0;
      let linearMemoryUnknownInstances = 0;
      for (const instance of shown) {
        if (instance.linearMemoryBytes === null) {
          linearMemoryUnknownInstances += 1;
          continue;
        }
        linearMemoryKnownInstances += 1;
        linearMemoryTotalBytes += instance.linearMemoryBytes;
      }

      return {
        generatedAt: new Date(generatedAtMs).toISOString(),
        uptimeSeconds: Math.max(0, Math.floor(deps.uptimeSeconds())),
        llm: {
          provider: config.model.provider,
          activeAlias: DEFAULT_MODEL_ALIAS,
          baseUrl: publicOrigin(config.model.baseUrl),
          tiers: config.model.tiers,
        },
        mcp: {
          remote: config.mcpServers.map((server) => ({
            name: server.name,
            url: publicOrigin(server.url),
            // 只输出名称；header 值（含 token）永不出网。
            headerNames: server.headers.map((header) => header.name),
          })),
        },
        wasm: {
          moduleUrl: displayModuleUrl(config.wasmModuleUrl),
          process: {
            rssBytes: memory.rss,
            heapTotalBytes: memory.heapTotal,
            heapUsedBytes: memory.heapUsed,
            arrayBufferBytes: memory.arrayBuffers,
          },
          chatHandles: registry.activeChatCount(),
          startingInstances: runtime.starting,
          unconfirmed: runtime.unconfirmed,
          ...(instancesError === undefined ? {} : { instancesError }),
          truncated,
          linearMemoryTotalBytes: linearMemoryKnownInstances === 0 ? null : linearMemoryTotalBytes,
          linearMemoryKnownInstances,
          linearMemoryUnknownInstances,
          instances: shown.map((instance) => mapWasmInstance(instance, generatedAtMs)),
        },
        sandbox: {
          enabled: config.sandbox.enabled,
          root: config.sandbox.root,
          settings: sandboxSettings,
          workspacesOnDisk: disk.count,
          ...(disk.error === undefined ? {} : { workspacesError: disk.error }),
          active: active.map((entry) => ({
            sandboxId: entry.sandboxId,
            state: entry.state,
            refs: entry.refs,
            startedAt: new Date(entry.startedAtMs).toISOString(),
            workspacePath: entry.workspacePath,
            mcpUrl: publicUrl(entry.mcpUrl),
            mcpServerName: entry.mcpServerName,
            lastError: entry.lastError
              ? { code: entry.lastError.code, message: truncate(entry.lastError.message) }
              : null,
          })),
          starting: pool?.starting ?? 0,
        },
      };
    },
  };
}
