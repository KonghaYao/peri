import { resolve, sep } from "node:path";
import { createApi } from "./api";
import { loadServerConfig, publicServerInfo, type ServerConfig } from "./config";
import { ChatManager } from "./agent";

/**
 * Bun 长驻服务：`/api/*` 由会话服务处理，其余路径提供 Vite 构建的前端静态资源。
 * 凭据只从进程环境读取；停止进程即结束所有运行中的 Agent 执行（不跨进程恢复）。
 */
function createStaticHandler(config: ServerConfig): (request: Request) => Promise<Response> {
  const root = resolve(config.staticRoot);
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

const loaded = loadServerConfig();
if (!loaded.config) {
  for (const problem of loaded.problems) console.error(`[peri-cf] ${problem}`);
  process.exit(1);
}
const config = loaded.config;
const manager = new ChatManager(config);
const app = createApi({ manager, info: publicServerInfo(config) });
const serveStatic = createStaticHandler(config);

const server = Bun.serve({
  hostname: config.hostname,
  port: config.port,
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/api" || path.startsWith("/api/")) return app.fetch(request);
    return serveStatic(request);
  },
});

console.log(`[peri-cf] server listening on http://${server.hostname}:${server.port}`);
console.log(`[peri-cf] model=${config.model.provider}/${config.model.id} mcp=${config.mcpServers.map((item) => item.name).join(",") || "none"}`);

let stopping = false;
for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.once(signal, async () => {
    if (stopping) return;
    stopping = true;
    server.stop();
    await manager.shutdown();
    process.exit(0);
  });
}
