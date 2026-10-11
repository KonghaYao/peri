/**
 * 浏览器本地配置：模型 API key、存储 URL/token 与 machineId 由用户在设置页填写，
 * 以单条 JSON 保存在 localStorage；Worker 零配置透传，凭据按真实值随请求上行。
 * 明文凭据只存在于本机浏览器，不得写入日志或其它持久化位置。
 */

export const CONFIG_STORAGE_KEY = "peri-cf.browser-config.v1";

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

export interface BrowserConfig {
  /** 客户端随机 UUID：Store 内的机器身份，首次生成后长期保存。 */
  machineId: string;
  model: ModelConfig;
  storage: StorageConfig;
}

/** Store 身份必须是规范的 UUID 文本，Rust 侧用 `Uuid::parse_str` 解析。 */
const MACHINE_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** localStorage 中未经验证的形状；逐字段取值时再收敛类型。 */
interface StoredConfig {
  machineId?: unknown;
  model?: { provider?: unknown; id?: unknown; baseUrl?: unknown; apiKey?: unknown };
  storage?: { url?: unknown; token?: unknown };
}

export function isModelProvider(value: unknown): value is ModelProvider {
  return typeof value === "string" && (MODEL_PROVIDERS as readonly string[]).includes(value);
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function readStored(): StoredConfig {
  const raw = globalThis.localStorage.getItem(CONFIG_STORAGE_KEY);
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

/** 读取本地配置；machineId 缺失或非 UUID 时重新生成并立即固化。 */
export function loadBrowserConfig(): BrowserConfig {
  const stored = readStored();
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
  };
  if (config.machineId !== storedId) saveBrowserConfig(config);
  return config;
}

export function saveBrowserConfig(config: BrowserConfig): void {
  globalThis.localStorage.setItem(CONFIG_STORAGE_KEY, JSON.stringify(config));
}

/** 必填校验；返回缺失项的中文名（空数组 = 可以启动宿主）。 */
export function configProblems(config: BrowserConfig): string[] {
  const problems: string[] = [];
  if (!config.model.id.trim()) problems.push("模型 ID");
  if (!config.model.baseUrl.trim()) problems.push("模型 Base URL");
  if (!config.model.apiKey.trim()) problems.push("模型 API Key");
  if (!config.storage.url.trim()) problems.push("存储 URL");
  if (!config.storage.token.trim()) problems.push("存储 Token");
  return problems;
}

export function isConfigured(config: BrowserConfig): boolean {
  return configProblems(config).length === 0;
}
