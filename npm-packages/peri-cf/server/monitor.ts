import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defaultSettingsPath, resolveSandboxSettings, type SandboxProfile } from "peri-sandbox";
import {
  SANDBOX_SERVER_NAME,
  publicOrigin,
  type ModelProvider,
  type ModelTier,
  type ServerConfig,
} from "./config";
import { DEFAULT_MODEL_ALIAS } from "./compose";
import {
  listSandboxWorkspaces,
  type SandboxPoolSnapshot,
  type SandboxRuntimeSnapshot,
} from "./sandbox";

/**
 * 只读服务端监控快照。
 *
 * 数据只来自「已有配置 + 内存事实 + 只读文件读取」：读取不会启动 sandbox 或 WASM
 * 运行后端，也不会执行 Agent。SDK 没有公开「每个 WASM 运行后端的堆占用」，因此
 * `wasm` 段只给进程级内存与活动句柄上限，不编造单实例数值。
 */

const SETTINGS_CACHE_TTL_MS = 30_000;
const WORKSPACE_CACHE_TTL_MS = 10_000;
const MAX_ERROR_MESSAGE_LENGTH = 300;
const MAX_DOMAINS = 100;

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
    sandboxEnabled: boolean;
    remote: Array<{ name: string; url: string; headerNames: string[] }>;
    sandbox: Array<{
      sandboxId: string;
      name: string;
      url: string | null;
      state: SandboxStateLabel;
    }>;
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
    /** 运行后端数量的上限；运行后端在首次 Action 才启动。 */
    activeChats: number;
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
          sandboxEnabled: config.sandbox.enabled,
          remote: config.mcpServers.map((server) => ({
            name: server.name,
            url: publicOrigin(server.url),
            // 只输出名称；header 值（含 token）永不出网。
            headerNames: server.headers.map((header) => header.name),
          })),
          sandbox: active.map((entry) => ({
            sandboxId: entry.sandboxId,
            name: entry.mcpServerName ?? SANDBOX_SERVER_NAME,
            url: publicUrl(entry.mcpUrl),
            state: entry.state,
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
          activeChats: registry.activeChatCount(),
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
