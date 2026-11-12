import type { MCPServer } from "mcp-use";
import { revision } from "./state.ts";

/** Hono 层：HTTP 中间件与自定义路由。 */
export function registerHttp(server: MCPServer): void {
  server.use("*", async (c, next) => {
    const startedAt = performance.now();
    const requestId = crypto.randomUUID();
    await next();
    c.header("x-request-id", requestId);
    c.header("x-demo-duration-ms", (performance.now() - startedAt).toFixed(2));
    console.log(`[http] ${c.req.method} ${c.req.path} -> ${c.res.status} (${requestId})`);
  });

  server.get("/health", (c) =>
    c.json({
      ok: true,
      server: "mcp-use-showcase",
      revision,
      uptimeSeconds: Math.round(performance.now() / 1000),
    }),
  );

  server.get("/api/version", (c) =>
    c.json({
      version: "0.1.0",
      protocolHint: "2026-07-28 stateless + 2025 legacy",
    }),
  );
}

/** MCP 层中间件：日志、审计、列表过滤。必须在工具注册前挂载。 */
export function registerMcp(server: MCPServer): void {
  server.use("mcp:*", async (ctx, next) => {
    const startedAt = performance.now();
    try {
      const result = await next();
      console.log(`[mcp] ${ctx.method} ${(performance.now() - startedAt).toFixed(2)}ms`);
      return result;
    } catch (error) {
      console.error(`[mcp] ${ctx.method} failed`, error);
      throw error;
    }
  });

  server.use("mcp:tools/call", async (ctx, next) => {
    const toolName = ctx.params?.name ?? "unknown";
    const userAgent = ctx.request?.header("user-agent") ?? "unknown";
    console.log(`[audit] tool=${toolName} ua=${userAgent}`);
    return next();
  });

  server.use("mcp:tools/list", async (_ctx, next) => {
    const tools = await next();
    // 演示列表过滤：内部诊断工具不进入发现面，但仍可以直接调用。
    return tools.filter((tool) => !tool.name.startsWith("_internal"));
  });
}
