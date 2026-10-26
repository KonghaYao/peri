import { resolve, sep } from "node:path";
import type { Server } from "bun";
import {
  SYNC_PATH_PATTERN,
  createApi,
  handleSyncUpgrade,
  syncWebSocketHandlers,
  type SyncSocketData,
} from "./api";
import {
  getChatManager,
  shutdownChatManager,
  type AppChatManager,
  type ChatPlatformConfig,
  type ServerChatOptions,
} from "./agent";
import { loadServerConfig, publicServerInfo, type ServerConfig } from "./config";

/**
 * Bun 长驻服务：`/api/*` 由会话服务处理，`/api/chats/:chatId/sync` 升级为只读
 * Yjs WebSocket，其余路径提供 Vite 构建的前端静态资源。
 *
 * 凭据只从进程环境读取；停止进程即结束所有运行中的 Agent 执行（不跨进程恢复）。
 */

export interface RunningServer {
  server: Server<SyncSocketData>;
  manager: AppChatManager;
}

/** HTTP/WS 层真正需要的配置切片；与 Agent 运行配置解耦。 */
export interface ServerWiring {
  hostname: string;
  port: number;
  staticRoot: string;
  info: ReturnType<typeof publicServerInfo>;
}

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
  wiring: ServerWiring,
  manager: AppChatManager,
): Server<SyncSocketData> {
  const logError = (
    label: string,
    error: unknown,
    context?: Record<string, unknown>,
  ): void => console.error(`[peri-cf] ${label}`, context ?? {}, error);
  const app = createApi({ manager, info: wiring.info, logError });
  const serveStatic = createStaticHandler(wiring.staticRoot);
  return Bun.serve<SyncSocketData>({
    hostname: wiring.hostname,
    port: wiring.port,
    async fetch(request, server) {
      const path = new URL(request.url).pathname;
      if (SYNC_PATH_PATTERN.test(path)) {
        // 成功升级时返回 undefined；失败在升级前返回 HTTP 响应。
        return handleSyncUpgrade(request, server, {
          manager,
          views: manager.views,
          logError,
        });
      }
      if (path === "/api" || path.startsWith("/api/")) return app.fetch(request);
      return serveStatic(request);
    },
    websocket: syncWebSocketHandlers,
  });
}

/** 启动入口是唯一知道完整 ServerConfig 的地方；Chat 平台只接收运行切片。 */
function chatPlatformConfig(config: ServerConfig): ChatPlatformConfig {
  const { machineId, model, storage, mcpServers, wasmModuleUrl } = config;
  return { machineId, model, storage, mcpServers, wasmModuleUrl };
}

/** 进程级入口：先取得单例 Manager，再启动服务。 */
export function startServer(
  config: ServerConfig,
  options: ServerChatOptions = {},
): RunningServer {
  const manager = getChatManager(chatPlatformConfig(config), options);
  const server = createServer({
    hostname: config.hostname,
    port: config.port,
    staticRoot: config.staticRoot,
    info: publicServerInfo(config),
  }, manager);
  return { server, manager };
}

function startFromEnvironment(): void {
  const loaded = loadServerConfig();
  if (!loaded.config) {
    for (const problem of loaded.problems) console.error(`[peri-cf] ${problem}`);
    process.exit(1);
  }
  const config = loaded.config;
  const { server } = startServer(config);
  console.log(`[peri-cf] server listening on http://${server.hostname}:${server.port}`);
  console.log(
    `[peri-cf] model=${config.model.provider}/${config.model.id}`
    + ` mcp=${config.mcpServers.map((item) => item.name).join(",") || "none"}`,
  );

  let stopping = false;
  const stop = async (): Promise<void> => {
    if (stopping) return;
    stopping = true;
    server.stop();
    try {
      await shutdownChatManager();
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
