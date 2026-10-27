import type { Server } from "bun";
import { ChatManager } from "@peri-code/sdk";
import { Hono } from "hono";
import {
  serveStatic,
  upgradeWebSocket,
  websocket,
  type BunWebSocketData,
} from "@hono/bun";
import {
  createApi,
  SYNC_PATH,
  syncEvents,
  syncUpgradeGuard,
  type SyncEnv,
} from "./api";
import { MODEL_TIERS, loadServerConfig, publicServerInfo, type ServerConfig } from "./config";
import { chatManagerOptions, defaultAgentConfig } from "./compose";
import { ChatRegistry } from "./registry";

/**
 * Bun 长驻服务：Hono 统一处理 `/api/*`、`/api/chats/:chatId/sync` 的 WS 升级
 * 与其余路径的前端静态资源（含 SPA 回退）。
 *
 * 凭据只从进程环境读取；停止进程即结束所有运行中的 Agent 执行（不跨进程恢复）。
 */

const NOT_FOUND_TEXT = "404 Not Found";

/** 组装路由；`fetch` 必须由 `Bun.serve` 提供 server 才能完成 WS 升级。 */
export function createApp(config: ServerConfig, registry: ChatRegistry): Hono<SyncEnv> {
  const app = new Hono<SyncEnv>();
  app.route("/", createApi({ registry, info: publicServerInfo(config) }));

  // 精确匹配的升级前置条件（405/426/410/500）与逐连接状态。
  app.all(SYNC_PATH, syncUpgradeGuard(registry));
  // 官方 helper 在 server.upgrade() 返回 false 时 next() 到同路由的 400 回退，
  // 不会落到下方的 `/api/*` 404。
  app.get(
    SYNC_PATH,
    upgradeWebSocket((c) => {
      const connection = c.get("syncConnection");
      if (!connection) throw new Error("sync upgrade guard did not run");
      return syncEvents(connection);
    }),
    (c) => c.text("WebSocket upgrade rejected", 400),
  );

  // 未匹配的 API 不得落到 SPA 回退。
  app.all("/api", (c) => c.notFound());
  app.all("/api/*", (c) => c.notFound());
  // 静态资源保持 `app.all`：改成 `app.get` 会让 POST/OPTIONS 未知路径从 200 变 404。
  app.all("*", serveStatic({ root: config.staticRoot }));
  app.all("*", serveStatic({ root: config.staticRoot, path: "index.html" }));
  app.notFound((c) => c.text(NOT_FOUND_TEXT, 404));
  return app;
}

/** 启动 HTTP/WS 服务；WASM 宿主仍由首个 Action 懒启动。 */
export function createServer(
  config: ServerConfig,
  registry: ChatRegistry,
): Server<BunWebSocketData> {
  return Bun.serve<BunWebSocketData>({
    hostname: config.hostname,
    port: config.port,
    fetch: createApp(config, registry).fetch,
    websocket,
  });
}

/** 进程级入口：构造注册表，再启动服务。 */
export function startServer(
  config: ServerConfig,
): { server: Server<BunWebSocketData>; registry: ChatRegistry } {
  const registry = new ChatRegistry(
    new ChatManager(chatManagerOptions(config)),
    defaultAgentConfig(config),
  );
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

  // SIGINT/SIGTERM 可能并发到达（终端进程组与 scripts/dev.ts 的 child.kill）。
  // ChatManager.shutdown 与 ChatView.dispose 都会合并并发关闭，因此不设防重入闸门；
  // 同一信号重复到达时 process.once 已移除监听，交给默认行为立即退出。
  const stop = async (): Promise<void> => {
    // 只停新准入；不等待 Promise。进程会在 registry.shutdown() 结算后退出。
    void server.stop();
    try {
      await registry.shutdown();
      process.exit(0);
    } catch (error) {
      console.error("[peri-cf] shutdown is incomplete; exiting", error);
      process.exit(1);
    }
  };
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.once(signal, () => { void stop(); });
  }
}

// 测试导入模块不得启动 WASM 宿主或监听端口。
if (import.meta.main) startFromEnvironment();
