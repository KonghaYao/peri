import type { Server } from "bun";
import { ChatManager } from "@peri-code/sdk";
import { Hono } from "hono";
import { getBunServer, serveStatic } from "hono/bun";
import {
  createApi,
  handleSyncUpgrade,
  syncWebSocketHandlers,
  type SyncSocketData,
} from "./api";
import { MODEL_TIERS, loadServerConfig, publicServerInfo, type ServerConfig } from "./config";
import { chatManagerOptions } from "./compose";
import { ChatRegistry } from "./registry";

/**
 * Bun 长驻服务：Hono 统一处理 `/api/*`、`/api/chats/:chatId/sync` 的 WS 升级
 * 与其余路径的前端静态资源（含 SPA 回退）。
 *
 * 凭据只从进程环境读取；停止进程即结束所有运行中的 Agent 执行（不跨进程恢复）。
 */

const NOT_FOUND_TEXT = "404 Not Found";

/** 组装 HTTP/WS 服务；WASM 宿主仍由首个 Action 懒启动。 */
export function createServer(
  config: ServerConfig,
  registry: ChatRegistry,
): Server<SyncSocketData> {
  const app = new Hono();
  app.route("/", createApi({ registry, info: publicServerInfo(config) }));
  app.all("/api/chats/:chatId/sync", async (c) => {
    const server = getBunServer<Server<SyncSocketData>>(c);
    if (!server) return c.text("WebSocket upgrade unavailable", 500);
    // 升级成功后由 Bun 接管连接；返回空响应与 Hono 的 Bun 适配器一致。
    return (await handleSyncUpgrade(c.req.raw, server, c.req.param("chatId"), registry))
      ?? new Response(null);
  });
  // 未匹配的 API 不得落到 SPA 回退。
  app.all("/api", () => new Response(NOT_FOUND_TEXT, { status: 404 }));
  app.all("/api/*", () => new Response(NOT_FOUND_TEXT, { status: 404 }));
  app.all("*", serveStatic({ root: config.staticRoot }));
  app.all("*", serveStatic({ root: config.staticRoot, path: "index.html" }));
  return Bun.serve<SyncSocketData>({
    hostname: config.hostname,
    port: config.port,
    fetch: app.fetch,
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
