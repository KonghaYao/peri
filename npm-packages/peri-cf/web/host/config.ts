import { BareHarnessConfig } from "@peri-code/sdk/portable";
import { parseMcpHeaders, type BrowserConfig } from "../settings/config";

/**
 * 浏览器宿主的运行配置：machineId、模型 key 与存储 token 全部来自浏览器本地配置
 * （`web/settings/config.ts`）；relay 是同源路径，上游目标由路径第一段携带，Worker 零配置透传。
 * MCP 服务器（插件页维护）同样经同源 relay 注入 settings：宿主只接 2026-07-28 协议
 * 版本的 MCP（浏览器部署限制见 peri-middlewares 的 MCP transport）。
 */

export const WORKSPACE_CWD = "/workspace";
export const STORAGE_RELAY_PATH = "/api/relay/storage";
export const MODEL_RELAY_PATH = "/api/relay/model";
export const MCP_RELAY_PATH = "/api/relay/mcp";

/** 上游基址编码成 relay 路径的第一段，Worker 据此选择转发目标。 */
export function storageRelayUrl(origin: string, upstream: string): string {
  return new URL(`${STORAGE_RELAY_PATH}/${encodeURIComponent(upstream)}`, origin).href;
}

export function modelRelayUrl(origin: string, upstream: string): string {
  return new URL(`${MODEL_RELAY_PATH}/${encodeURIComponent(upstream)}`, origin).href;
}

export function mcpRelayUrl(origin: string, upstream: string): string {
  return new URL(`${MCP_RELAY_PATH}/${encodeURIComponent(upstream)}`, origin).href;
}

/**
 * 注入宿主的 MCP server 表（settings.config.mcpServers）：每个条目的 URL 改写为
 * 同源 relay 前缀，请求头文本解析成键值表。条目在设置页已校验；这里对空名与
 * 无法解析的请求头保持防御性跳过。
 */
function mcpServersConfig(config: BrowserConfig, origin: string): Record<string, { url: string; headers?: Record<string, string> }> {
  const servers: Record<string, { url: string; headers?: Record<string, string> }> = {};
  for (const entry of config.mcpServers ?? []) {
    const name = entry.name.trim();
    if (!name) continue;
    const headers = parseMcpHeaders(entry.headers);
    servers[name] = {
      url: mcpRelayUrl(origin, entry.url.trim()),
      ...(headers && Object.keys(headers).length > 0 ? { headers } : {}),
    };
  }
  return servers;
}

export function harnessConfigJson(config: BrowserConfig, origin: string): string {
  return JSON.stringify({
    cwd: WORKSPACE_CWD,
    machineId: config.machineId,
    storage: { url: storageRelayUrl(origin, config.storage.url), authToken: config.storage.token },
    settings: { config: {
      active_alias: "sonnet",
      providers: [{
        id: "model", type: config.model.provider, apiKey: config.model.apiKey,
        baseUrl: modelRelayUrl(origin, config.model.baseUrl),
      }],
      profiles: { sonnet: { provider: "model", model: config.model.id } },
      // 浏览器部署只开 MCP 工具面（其余能力保持裸 harness 的关闭默认）。
      meta_harness: { ...BareHarnessConfig, McpMiddleware: true },
      mcpServers: mcpServersConfig(config, origin),
    } },
  });
}
