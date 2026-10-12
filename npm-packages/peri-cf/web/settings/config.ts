/**
 * 浏览器本地配置：模型 API key、存储 URL/token、machineId 与 MCP 服务器（插件）。
 * 全部装在一个文件里（`/config.json`），落在浏览器 VFS 的 IndexedDB 持久层。
 * 读取方只有浏览器侧（设置/插件页、宿主启动时的 settings 注入）；Rust 侧不读该文件。
 * Worker 零配置透传，凭据按真实值随请求上行。
 * 明文凭据只存在于本机浏览器，不得写入日志或其它持久化位置。
 */

import type { Vfs } from "../vfs";

/** 配置文件在 VFS 中的路径：一个文件装全部。 */
export const CONFIG_PATH = "/config.json";

export const MODEL_PROVIDERS = ["openai", "anthropic"] as const;
export type ModelProvider = (typeof MODEL_PROVIDERS)[number];

export interface ModelConfig {
  provider: ModelProvider;
  id: string;
  baseUrl: string;
  apiKey: string;
}

export interface StorageConfig {
  url: string;
  token: string;
}

/**
 * 一条 MCP 服务器（插件）配置。`name` 同时是宿主内的 server 名（工具名
 * `mcp__{name}__{tool}` 的前缀），因此在会话内必须唯一；`url` 是 MCP server
 * 的真实绝对地址，宿主启动时改写为同源 relay 前缀（`/api/relay/mcp/...`）。
 * `headers` 是每行一条 `Key: Value` 的自定义请求头文本，空串表示无。
 */
export interface McpServerEntry {
  name: string;
  url: string;
  headers: string;
}

export interface BrowserConfig {
  /** 客户端随机 UUID：Store 内的机器身份，首次生成后长期保存。 */
  machineId: string;
  model: ModelConfig;
  storage: StorageConfig;
  /** MCP 服务器列表（插件页维护）；空数组表示未接入任何 MCP。 */
  mcpServers: McpServerEntry[];
}

/** MCP server 名：宿主内成为工具名前缀，限制为字母、数字、下划线与连字符。 */
const MCP_NAME_PATTERN = /^[A-Za-z0-9_-]+$/;

/** Store 身份必须是规范的 UUID 文本，Rust 侧用 `Uuid::parse_str` 解析。 */
const MACHINE_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** 配置文件中未经验证的形状；逐字段取值时再收敛类型。 */
interface StoredConfig {
  machineId?: unknown;
  model?: { provider?: unknown; id?: unknown; baseUrl?: unknown; apiKey?: unknown };
  storage?: { url?: unknown; token?: unknown };
  mcpServers?: unknown;
}

export function isModelProvider(value: unknown): value is ModelProvider {
  return typeof value === "string" && (MODEL_PROVIDERS as readonly string[]).includes(value);
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** 逐条收敛配置文件中的 MCP 条目；无法识别的条目按空值（由校验拦下）。 */
function readMcpServers(value: unknown): McpServerEntry[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item) => item && typeof item === "object").map((item) => {
    const entry = item as { name?: unknown; url?: unknown; headers?: unknown };
    return { name: text(entry.name), url: text(entry.url), headers: text(entry.headers) };
  });
}

async function readStored(vfs: Vfs): Promise<StoredConfig> {
  const raw = await vfs.readText(CONFIG_PATH);
  if (!raw) return {};
  try {
    const value: unknown = JSON.parse(raw);
    return value && typeof value === "object" ? (value as StoredConfig) : {};
  } catch {
    // 只报告解析失败，不回显原文，避免把 token 写进控制台；按未配置处理。
    console.warn("浏览器配置无法解析，请在设置页重新填写。");
    return {};
  }
}

/** 读取配置；machineId 缺失或非 UUID 时重新生成并立即固化。 */
export async function loadBrowserConfig(vfs: Vfs): Promise<BrowserConfig> {
  const stored = await readStored(vfs);
  const storedId = text(stored.machineId);
  const config: BrowserConfig = {
    machineId: MACHINE_ID_PATTERN.test(storedId) ? storedId : globalThis.crypto.randomUUID(),
    model: {
      provider: isModelProvider(stored.model?.provider) ? stored.model.provider : "openai",
      id: text(stored.model?.id),
      baseUrl: text(stored.model?.baseUrl),
      apiKey: text(stored.model?.apiKey),
    },
    storage: { url: text(stored.storage?.url), token: text(stored.storage?.token) },
    mcpServers: readMcpServers(stored.mcpServers),
  };
  if (config.machineId !== storedId) await saveBrowserConfig(vfs, config);
  return config;
}

export async function saveBrowserConfig(vfs: Vfs, config: BrowserConfig): Promise<void> {
  await vfs.writeText(CONFIG_PATH, JSON.stringify(config));
}

/** relay 只按 http(s) 选择并转发上游：`libsql://`、`turso://` 会被 Worker 直接拒绝。 */
function isHttpAddress(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

/** 上游地址：空值按必填报出，非空但非 http(s) 报格式，二者都在设置页可见。 */
function addressProblems(label: string, value: string): string[] {
  if (!value.trim()) return [label];
  return isHttpAddress(value) ? [] : [`${label}（须为 http(s) 地址）`];
}

/**
 * 解析每行一条 `Key: Value` 的自定义请求头文本；空行忽略。
 * 返回 null 表示存在非法行（缺 Key 或没有分隔冒号），由 [`configProblems`] 报出。
 */
export function parseMcpHeaders(text: string): Record<string, string> | null {
  const headers: Record<string, string> = {};
  for (const rawLine of text.split("\n")) {
    const line = rawLine.trim();
    if (!line) continue;
    const separator = line.indexOf(":");
    const name = separator === -1 ? "" : line.slice(0, separator).trim();
    if (!name) return null;
    headers[name] = line.slice(separator + 1).trim();
  }
  return headers;
}

/**
 * MCP 条目校验（可选配置：条目一旦存在就必须合法）。
 * 每条按名称（必填、字符集、唯一）、URL（必填、http(s)）与请求头解析逐项检查。
 */
function mcpProblems(servers: McpServerEntry[]): string[] {
  const problems: string[] = [];
  const seen = new Set<string>();
  servers.forEach((server, index) => {
    const label = `MCP #${index + 1}`;
    const name = server.name.trim();
    if (!name) problems.push(`${label} 名称`);
    else if (!MCP_NAME_PATTERN.test(name)) problems.push(`${label} 名称（仅字母、数字、_ 与 -）`);
    else if (seen.has(name)) problems.push(`${label} 名称重复`);
    else seen.add(name);
    problems.push(...addressProblems(`${label} URL`, server.url));
    if (parseMcpHeaders(server.headers) === null)
      problems.push(`${label} 请求头（每行须为 Key: Value）`);
  });
  return problems;
}

/** 必填与格式校验；返回待处理项的中文名（空数组 = 可以启动宿主）。 */
export function configProblems(config: BrowserConfig): string[] {
  const problems: string[] = [];
  if (!config.model.id.trim()) problems.push("模型 ID");
  problems.push(...addressProblems("模型 Base URL", config.model.baseUrl));
  if (!config.model.apiKey.trim()) problems.push("模型 API Key");
  problems.push(...addressProblems("存储 URL", config.storage.url));
  if (!config.storage.token.trim()) problems.push("存储 Token");
  problems.push(...mcpProblems(config.mcpServers ?? []));
  return problems;
}

export function isConfigured(config: BrowserConfig): boolean {
  return configProblems(config).length === 0;
}
