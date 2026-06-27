// ============ 工作区 API + 静态文件服务 ============
import type { Hono } from "hono";
import { join, extname } from "node:path";
import { stat, readFile } from "node:fs/promises";
import { workspaceState, deepMerge, saveWorkspaceFile } from "../lib/workspace.js";
import type { TerminalSessionMeta } from "../types.js";

export function registerWorkspaceRoutes(
  app: Hono,
  publicDir: string,
  terminalSessions: Map<string, { alive: boolean; cols: number; rows: number; createdAt: number }>,
) {
  // GET 获取完整工作区状态（含活跃终端列表）
  app.get("/api/workspace", (c) => {
    try {
      const terminalList: TerminalSessionMeta[] = [];
      terminalSessions.forEach((s, id) => {
        if (s.alive) terminalList.push({ id, cols: s.cols, rows: s.rows, createdAt: s.createdAt });
      });
      return c.json({ ...workspaceState, terminals: terminalList });
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  // POST 部分更新工作区状态（deep merge）
  app.post("/api/workspace", async (c) => {
    try {
      const body = await c.req.json();
      const newState = deepMerge(workspaceState, body) as typeof workspaceState;
      Object.assign(workspaceState, newState);
      saveWorkspaceFile(workspaceState);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ error: err.message }, 400);
    }
  });

  // 静态文件: public/ 兜底
  app.get("/*", async (c) => {
    const reqPath = c.req.path === "/" ? "/index.html" : c.req.path;
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
