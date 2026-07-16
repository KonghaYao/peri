// ============ 工作区 API + 静态文件服务（瘦身版） ============
import type { Hono } from "hono";
import { join, extname } from "node:path";
import { stat, readFile } from "node:fs/promises";
import type { WorkspaceService } from "../services/workspace-service.js";

export function registerWorkspaceRoutes(
  app: Hono,
  workspaceService: WorkspaceService,
  publicDir: string,
) {
  app.get("/api/workspace", (c) => {
    try { return c.json(workspaceService.getState()); }
    catch (err: any) { return c.json({ error: err.message }, 500); }
  });

  app.post("/api/workspace", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(workspaceService.updateState(body));
    } catch (err: any) { return c.json({ error: err.message }, 400); }
  });

  // 子键 GET：GET /api/workspace/:key
  app.get("/api/workspace/:key", (c) => {
    try {
      const key = c.req.param("key");
      const val = workspaceService.getKey(key);
      return val === undefined ? c.json({ error: "not found" }, 404) : c.json(val);
    } catch (err: any) { return c.json({ error: err.message }, 500); }
  });

  // 子键 PATCH（merge 语义）：PATCH /api/workspace/:key
  app.patch("/api/workspace/:key", async (c) => {
    try {
      const key = c.req.param("key");
      const patch = await c.req.json();
      return c.json(workspaceService.setKey(key, patch));
    } catch (err: any) { return c.json({ error: err.message }, 400); }
  });

  // 静态文件: public/ 兜底
  app.get("/*", async (c) => {
    const reqPath = c.req.path === "/" ? "/parent.html" : c.req.path;
    const filePath = join(publicDir, reqPath);
    try {
      const statInfo = await stat(filePath);
      if (statInfo.isFile()) {
        const content = await readFile(filePath);
        const ext = extname(filePath).toLowerCase();
        const mimeMap: Record<string, string> = {
          ".html": "text/html; charset=utf-8", ".css": "text/css; charset=utf-8",
          ".js": "application/javascript; charset=utf-8", ".mjs": "application/javascript; charset=utf-8",
          ".json": "application/json", ".png": "image/png",
          ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
          ".svg": "image/svg+xml", ".ico": "image/x-icon",
          ".woff2": "font/woff2", ".map": "application/json",
        };
        return c.body(content, 200, { "Content-Type": mimeMap[ext] || "application/octet-stream" });
      }
    } catch {}
    return c.notFound();
  });
}
