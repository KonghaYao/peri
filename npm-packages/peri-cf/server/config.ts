import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import type { AcpMcpServer, PeriConfig, TursoStorage } from "@peri-code/sdk";

/** 宿主虚拟工作区的根；与 ACP 会话 cwd 分开（见 `SESSION_CWD`）。 */
export const WORKSPACE_CWD = "/workspace";
/**
 * 历史 Store 中带尾斜杠的会话 cwd。目录读取仍把它作为 `currentCwd`，`/api/info`
 * 也继续展示该值；新会话 cwd 由 `workspace.runtimePath` 派生并归一化为
 * `/workspace`（不再保留尾斜杠）。同一次读取还以 `WORKSPACE_CWD` 兼容新值。
 */
export const SESSION_CWD = "/workspace/";

const MACHINE_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const HEADER_NAME_PATTERN = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
/** 档位与供应商联合直接取自 SDK 启动对象，避免两处漂移。 */
export type ModelProvider = NonNullable<PeriConfig["config"]["providers"]>[number]["type"];
export type ModelTier = NonNullable<PeriConfig["config"]["active_alias"]>;

const MODEL_PROVIDERS = ["openai", "anthropic"] as const satisfies readonly ModelProvider[];
/** 与 peri-config 的 `Profiles` 四档一致；子 Agent 按档位取模型。 */
export const MODEL_TIERS = ["fable", "opus", "sonnet", "haiku"] as const satisfies readonly ModelTier[];

/** 服务端进程配置；进程内字段形状只在这里声明一次。 */
export interface ServerConfig {
  hostname: string;
  port: number;
  machineId: string;
  model: {
    provider: ModelProvider;
    baseUrl: string;
    apiKey: string;
    /** 四档模型 ID；未单独配置的档位回退 `PERI_MODEL_ID`。 */
    tiers: Record<ModelTier, string>;
  };
  /** 与 SDK `TursoStorage#storageConfig` 同形；目录 cwd 由 `compose.ts` 补齐。 */
  storage: TursoStorage["storageConfig"];
  /** 只支持远程 Streamable HTTP；本地 ACP/stdio 不随后端执行。 */
  mcpServers: { name: string; url: string; headers: { name: string; value: string }[] }[];
  /** 服务端 Emscripten 胶水位置（file URL 或 http(s) URL）。 */
  wasmModuleUrl: string;
  /** 生产环境的前端静态目录；不存在时只提供 API。 */
  staticRoot: string;
}

export type ServerEnvironment = Readonly<NodeJS.ProcessEnv>;

const DEFAULT_PORT = 8787;
const DEFAULT_HOSTNAME = "127.0.0.1";

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function isHttpAddress(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

function parsePort(value: string | undefined): number | undefined {
  if (!value) return DEFAULT_PORT;
  const port = Number(value);
  return Number.isInteger(port) && port > 0 && port <= 65_535 ? port : undefined;
}

function parseModel(env: ServerEnvironment, problems: string[]): ServerConfig["model"] | undefined {
  const provider = text(env.PERI_MODEL_PROVIDER) || "openai";
  const id = text(env.PERI_MODEL_ID);
  const baseUrl = text(env.PERI_MODEL_BASE_URL);
  const apiKey = text(env.PERI_MODEL_API_KEY);
  const providerValid = (MODEL_PROVIDERS as readonly string[]).includes(provider);
  const baseUrlValid = !baseUrl || isHttpAddress(baseUrl);
  if (!providerValid) problems.push(`PERI_MODEL_PROVIDER 必须是 ${MODEL_PROVIDERS.join(" 或 ")}`);
  if (!id) problems.push("缺少 PERI_MODEL_ID");
  if (!baseUrl) problems.push("缺少 PERI_MODEL_BASE_URL");
  else if (!baseUrlValid) problems.push("PERI_MODEL_BASE_URL 必须是 http(s) 地址");
  if (!apiKey) problems.push("缺少 PERI_MODEL_API_KEY");
  if (!providerValid || !id || !baseUrl || !baseUrlValid || !apiKey) return undefined;
  const tiers = Object.fromEntries(
    MODEL_TIERS.map((tier) => [tier, text(env[`PERI_MODEL_ID_${tier.toUpperCase()}`]) || id]),
  ) as Record<ModelTier, string>;
  return { provider: provider as ModelProvider, tiers, baseUrl, apiKey };
}

function parseStorage(env: ServerEnvironment, problems: string[]): ServerConfig["storage"] | undefined {
  const url = text(env.PERI_STORAGE_URL);
  const authToken = typeof env.PERI_STORAGE_TOKEN === "string" ? env.PERI_STORAGE_TOKEN : "";
  if (!url) problems.push("缺少 PERI_STORAGE_URL");
  else if (!isHttpAddress(url)) problems.push("PERI_STORAGE_URL 必须是 http(s) 地址");
  if (!authToken) problems.push("缺少 PERI_STORAGE_TOKEN");
  return url && authToken ? { url, authToken } : undefined;
}

interface RawMcpServer {
  name?: unknown;
  url?: unknown;
  token?: unknown;
  headers?: unknown;
}

function parseMcpHeaders(raw: unknown, serverName: string, problems: string[]): { name: string; value: string }[] {
  if (raw === undefined) return [];
  if (!Array.isArray(raw)) {
    problems.push(`PERI_MCP_SERVERS[${serverName}].headers 必须是数组`);
    return [];
  }
  const headers: { name: string; value: string }[] = [];
  for (const entry of raw) {
    if (!entry || typeof entry !== "object") {
      problems.push(`PERI_MCP_SERVERS[${serverName}].headers 含非法条目`);
      continue;
    }
    const header = entry as { name?: unknown; value?: unknown };
    const name = text(header.name);
    const value = typeof header.value === "string" ? header.value : "";
    if (!HEADER_NAME_PATTERN.test(name) || /[\r\n]/.test(value)) {
      problems.push(`PERI_MCP_SERVERS[${serverName}] 含非法 header`);
      continue;
    }
    headers.push({ name, value });
  }
  return headers;
}

/** `PERI_MCP_SERVERS` 是 JSON 数组；只接受远程 HTTP，token 是 Authorization 头的简写。 */
function parseMcpServers(env: ServerEnvironment, problems: string[]): ServerConfig["mcpServers"] {
  const raw = text(env.PERI_MCP_SERVERS);
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    problems.push("PERI_MCP_SERVERS 不是合法 JSON");
    return [];
  }
  if (!Array.isArray(parsed)) {
    problems.push("PERI_MCP_SERVERS 必须是数组");
    return [];
  }
  const servers: ServerConfig["mcpServers"] = [];
  const names = new Set<string>();
  for (const entry of parsed) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      problems.push("PERI_MCP_SERVERS 含非法条目");
      continue;
    }
    const rawServer = entry as RawMcpServer;
    const name = text(rawServer.name);
    const url = text(rawServer.url);
    if (!name) {
      problems.push("PERI_MCP_SERVERS 条目缺少 name");
      continue;
    }
    if (names.has(name)) {
      problems.push(`PERI_MCP_SERVERS 名称重复：${name}`);
      continue;
    }
    if (!isHttpAddress(url)) {
      problems.push(`PERI_MCP_SERVERS[${name}] 必须是 http(s) URL`);
      continue;
    }
    const headers = parseMcpHeaders(rawServer.headers, name, problems);
    const token = typeof rawServer.token === "string" ? rawServer.token.trim() : "";
    if (token && !headers.some((header) => header.name.toLowerCase() === "authorization"))
      headers.push({ name: "Authorization", value: `Bearer ${token}` });
    names.add(name);
    servers.push({ name, url, headers });
  }
  return servers;
}

function parseWasmModuleUrl(env: ServerEnvironment, packageRoot: string): string {
  const raw = text(env.PERI_WASM_MODULE_URL);
  if (raw && (raw.startsWith("file:") || isHttpAddress(raw))) return raw;
  const path = raw ? resolve(packageRoot, raw) : resolve(packageRoot, "wasm/generated/peri-wasm.js");
  return pathToFileURL(path).href;
}

/**
 * 服务端配置全部来自环境变量；密钥不进入前端，也不写入日志。
 * 返回问题清单而不是抛错，便于启动入口一次性报告所有缺失项。
 */
export function loadServerConfig(
  env: ServerEnvironment = process.env,
  packageRoot = resolve(import.meta.dir, ".."),
): { config?: ServerConfig; problems: string[] } {
  const problems: string[] = [];
  const port = parsePort(text(env.PERI_SERVER_PORT));
  if (port === undefined) problems.push("PERI_SERVER_PORT 必须是 1..65535 的整数");
  const machineId = text(env.PERI_MACHINE_ID);
  if (!machineId) problems.push("缺少 PERI_MACHINE_ID");
  else if (!MACHINE_ID_PATTERN.test(machineId)) problems.push("PERI_MACHINE_ID 必须是 UUID");

  const model = parseModel(env, problems);
  const storage = parseStorage(env, problems);
  const mcpServers = parseMcpServers(env, problems);

  // 任何解析问题都拒绝启动：宁可明确失败，也不带着缺失的 MCP/凭据执行。
  if (problems.length > 0 || !model || !storage || port === undefined || !MACHINE_ID_PATTERN.test(machineId))
    return { problems };

  return {
    problems,
    config: {
      hostname: text(env.PERI_SERVER_HOST) || DEFAULT_HOSTNAME,
      port,
      machineId,
      model,
      storage,
      mcpServers,
      wasmModuleUrl: parseWasmModuleUrl(env, packageRoot),
      staticRoot: text(env.PERI_STATIC_ROOT)
        ? resolve(packageRoot, text(env.PERI_STATIC_ROOT))
        : resolve(packageRoot, "dist/client"),
    },
  };
}

/**
 * Chat 默认 MCP 声明：后端只产生远程 HTTP 条目。
 *
 * 每次调用返回独立对象；服务端配置仍是唯一来源，ChatManager 只在构造时快照默认值。
 */
export function mcpServerDefaults(
  config: Pick<ServerConfig, "mcpServers">,
): Record<string, AcpMcpServer> {
  return Object.fromEntries(config.mcpServers.map((server) => [server.name, {
    type: "http" as const,
    url: server.url,
    headers: Object.fromEntries(
      server.headers.map((header) => [header.name, header.value]),
    ),
  }]));
}

/** 不暴露 token / query / userinfo 的配置摘要，供前端设置页只读展示。 */
export function publicServerInfo(
  config: Pick<ServerConfig, "model" | "storage" | "mcpServers">,
) {
  const origin = (raw: string) => {
    try {
      const url = new URL(raw);
      return `${url.protocol}//${url.host}${url.pathname}`;
    } catch {
      return "";
    }
  };
  return {
    model: { provider: config.model.provider, tiers: config.model.tiers, baseUrl: origin(config.model.baseUrl) },
    storage: { url: origin(config.storage.url) },
    mcp: config.mcpServers.map((server) => ({ name: server.name, url: origin(server.url) })),
    workspace: WORKSPACE_CWD,
    sessionCwd: SESSION_CWD,
  };
}

/** `GET /api/info` 的去敏响应；形状由实现推导，避免再维护一份声明。 */
export type ServerInfo = ReturnType<typeof publicServerInfo>;
