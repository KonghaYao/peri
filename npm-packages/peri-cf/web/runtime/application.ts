import { startBrowserHost } from "../host";
import type { BrowserConfig } from "../settings/config";
import { createPeriSession } from "./creation";
import { ChatRegistry } from "./registry";
import { chatRepository } from "./repository";
import type { ChatRuntime } from "./session";
import type { ChatRepository, StartHost } from "./types";

export interface PeriApplication {
  readonly repository: ChatRepository;
  runtimeFor(chatId: string): ChatRuntime;
}

export interface PeriApplicationOptions {
  /** 页面启动时读到的浏览器配置（凭据、MCP 服务器）；应用不自行读盘。 */
  config: BrowserConfig;
  /** 同源 base；测试可注入。 */
  origin?: string;
  /** 宿主启动 seam；测试可注入替身。 */
  startHost?: StartHost;
}

/**
 * 标签页应用的单例装配：宿主按需启动（每次执行/复原一个实例，用完关闭），
 * 会话清单与创建经同源 relay；本对象在应用生命周期内保持同一 registry 与仓储。
 */
export function createApplication(options: PeriApplicationOptions): PeriApplication {
  const { config } = options;
  const origin = options.origin ?? globalThis.location.origin;
  const startHost: StartHost = options.startHost ?? ((hostOptions) => startBrowserHost({ ...hostOptions, origin, config }));
  const registry = new ChatRegistry();
  const repository = chatRepository(origin, config, (title) => createPeriSession(title, startHost));
  return {
    repository,
    runtimeFor: (chatId) => registry.runtimeFor(chatId, { startHost, repository }),
  };
}
