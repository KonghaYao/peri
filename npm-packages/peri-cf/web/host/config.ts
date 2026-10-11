import { BareHarnessConfig } from "@peri-code/sdk/portable";
import type { BrowserConfig } from "../settings/config";

/**
 * 浏览器宿主的运行配置：machineId、模型 key 与存储 token 全部来自浏览器本地配置
 * （`web/settings/config.ts`）；relay 是同源路径，上游目标由路径第一段携带，Worker 零配置透传。
 */

export const WORKSPACE_CWD = "/workspace";
export const STORAGE_RELAY_PATH = "/api/relay/storage";
export const MODEL_RELAY_PATH = "/api/relay/model";

/** 上游基址编码成 relay 路径的第一段，Worker 据此选择转发目标。 */
export function storageRelayUrl(origin: string, upstream: string): string {
  return new URL(`${STORAGE_RELAY_PATH}/${encodeURIComponent(upstream)}`, origin).href;
}

export function modelRelayUrl(origin: string, upstream: string): string {
  return new URL(`${MODEL_RELAY_PATH}/${encodeURIComponent(upstream)}`, origin).href;
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
      meta_harness: BareHarnessConfig,
    } },
  });
}
