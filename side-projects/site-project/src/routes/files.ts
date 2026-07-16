// ============ 文件树 / 文件读取 / 文件状态 路由 ============
import type { Hono } from "hono";
import type { FileService } from "../services/file-service.js";

export function registerFileRoutes(app: Hono, fileService: FileService) {
  // API: 文件树（支持 ?path= 懒加载子树）
  app.get("/api/tree", async (c) => {
    try {
      const subPath = c.req.query("path") || "";
      const nodes = await fileService.getTree(subPath);
      if (nodes.error) return c.json({ error: nodes.error }, 403);
      return c.json(nodes);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  // API: 读取文件内容
  app.get("/api/file", async (c) => {
    try {
      const filePath = c.req.query("path");
      const result = await fileService.getFile(filePath || "");
      if (result.error) {
        const code = result.error === "缺少 path 参数" ? 400 : result.error === "非法路径" ? 403 : result.error === "不能读取目录" ? 400 : 404;
        return c.json({ error: result.error }, code);
      }
      if (result.binary) {
        return new Response(result.binary, { headers: { "Content-Type": result.mime || "application/octet-stream" } });
      }
      return c.json({ content: result.content, language: result.language, size: result.size, mtime: result.mtime });
    } catch (err: any) {
      return c.json({ error: err.message }, 404);
    }
  });

  // API: 文件状态（轻量轮询用）
  app.get("/api/stat", async (c) => {
    try {
      const filePath = c.req.query("path");
      const result = await fileService.getStat(filePath || "");
      if (result.error) {
        const code = result.error === "缺少 path 参数" ? 400 : result.error === "非法路径" ? 403 : 400;
        return c.json({ error: result.error }, code);
      }
      return c.json(result);
    } catch (err: any) {
      return c.json({ error: err.message }, 404);
    }
  });
}
