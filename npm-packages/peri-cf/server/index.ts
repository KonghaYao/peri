import { resolve, sep } from "node:path";
import type { Server } from "bun";
import { ChatManager } from "@peri-code/sdk";
import {
  SYNC_PATH_PATTERN,
  createApi,
  handleSyncUpgrade,
  syncWebSocketHandlers,
  type SyncSocketData,
} from "./api";
import { MODEL_TIERS, loadServerConfig, publicServerInfo, type ServerConfig } from "./config";
import { chatManagerOptions } from "./compose";
import { ChatRegistry } from "./registry";

/**
 * Bun 长驻服务：`/api/*` 由会话服务处理，`/api/chats/:chatId/sync` 升级为只读
 * Yjs WebSocket，其余路径提供 Vite 构建的前端静态资源。
 *
 * 凭据只从进程环境读取；停止进程即结束所有运行中的 Agent 执行（不跨进程恢复）。
 */

function createStaticHandler(staticRoot: string): (request: Request) => Promise<Response> {
  const root = resolve(staticRoot);
  const index = Bun.file(resolve(root, "index.html"));
  return async (request) => {
    const pathname = decodeURIComponent(new URL(request.url).pathname);
    const target = resolve(root, `.${pathname}`);
    if (target !== root && !target.startsWith(`${root}${sep}`))
      return new Response("Not found", { status: 404 });
    const file = Bun.file(target);
    if (await file.exists()) return new Response(file);
    if (await index.exists())
      return new Response(index, { headers: { "content-type": "text/html; charset=utf-8" } });
    return new Response("Not found", { status: 404 });
  };
}

/** 组装 HTTP/WS 服务；WASM 宿主仍由首个 Action 懒启动。 */
export function createServer(
  config: ServerConfig,
  registry: ChatRegistry,
): Server<SyncSocketData> {
  const app = createApi({ registry, info: publicServerInfo(config) });
  const serveStatic = createStaticHandler(config.staticRoot);
  return Bun.serve<SyncSocketData>({
    hostname: config.hostname,
    port: config.port,
    async fetch(request, server) {
      const path = new URL(request.url).pathname;
      if (SYNC_PATH_PATTERN.test(path)) {
        // 成功升级时返回 undefined；失败在升级前返回 HTTP 响应。
        return handleSyncUpgrade(request, server, registry);
      }
      if (path === "/api" || path.startsWith("/api/")) return app.fetch(request);
      return serveStatic(request);
    },
    websocket: syncWebSocketHandlers,
  });
}

/** 进程级入口：构造注册表，再启动服务。 */
export function startServer(
  config: ServerConfig,
): { server: Server<SyncSocketData>; registry: ChatRegistry } {
  const registry = new ChatRegistry(new ChatManager(chatManagerOptions(config)));
  return { server: createServer(config, registry), registry };
}

function startFromEnvironment(): void {
  const loaded = loadServerConfig();
  if (!loaded.config) {
    for (const problem of loaded.problems) console.error(`[peri-cf] ${problem}`);
    process.exit(1);
  }
  const config = loaded.config;
  const { server, registry } = startServer(config);
  console.log(`[peri-cf] server listening on http://${server.hostname}:${server.port}`);
  console.log(
    `[peri-cf] model=${config.model.provider}`
    + ` tiers=${MODEL_TIERS.map((tier) => `${tier}:${config.model.tiers[tier]}`).join(",")}`
    + ` mcp=${config.mcpServers.map((item) => item.name).join(",") || "none"}`,
  );

  let stopping = false;
  const stop = async (): Promise<void> => {
    if (stopping) return;
    stopping = true;
    server.stop();
    try {
      await registry.shutdown();
      process.exit(0);
    } catch (error) {
      console.error("[peri-cf] shutdown is incomplete; resources are preserved", error);
      process.exit(1);
    }
  };
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.once(signal, () => { void stop(); });
  }
}

// 测试导入模块不得启动 WASM 宿主或监听端口。
if (import.meta.main) startFromEnvironment();
