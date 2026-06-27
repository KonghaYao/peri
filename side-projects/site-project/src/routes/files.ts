// ============ 文件树 / 文件读取 / 文件状态 路由 ============
import type { Hono } from "hono";
import { join, extname } from "node:path";
import { stat, readFile } from "node:fs/promises";
import {
  loadGitignore, setGitignoreFilter, getGitignoreFilter,
  listDir, isTextFile, extToLang, getMime,
} from "../lib/workspace.js";

export function registerFileRoutes(app: Hono, docsDir: string) {
  // API: 文件树（支持 ?path= 懒加载子树）
  app.get("/api/tree", async (c) => {
    try {
      if (!getGitignoreFilter()) {
        setGitignoreFilter(await loadGitignore(docsDir));
      }

      const subPath = c.req.query("path") || "";
      const targetDir = subPath ? join(docsDir, subPath) : docsDir;

      if (!targetDir.startsWith(docsDir)) {
        return c.json({ error: "非法路径" }, 403);
      }

      const nodes = await listDir(targetDir, docsDir);
      return c.json(nodes);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  // API: 读取文件内容
  app.get("/api/file", async (c) => {
    const filePath = c.req.query("path");
    if (!filePath) return c.json({ error: "缺少 path 参数" }, 400);

    const fullPath = join(docsDir, filePath);
    if (!fullPath.startsWith(docsDir)) {
      return c.json({ error: "非法路径" }, 403);
    }

    try {
      const info = await stat(fullPath);
      if (info.isDirectory()) {
        return c.json({ error: "不能读取目录" }, 400);
      }

      if (isTextFile(filePath)) {
        const text = await readFile(fullPath, "utf-8");
        return c.json({
          content: text,
          language: extToLang(extname(filePath)),
          size: info.size,
          mtime: info.mtimeMs,
        });
      }

      const buf = await readFile(fullPath);
      return new Response(buf, {
        headers: { "Content-Type": getMime(filePath) },
      });
    } catch (err: any) {
      return c.json({ error: err.message }, 404);
    }
  });

  // API: 文件状态（轻量轮询用）
  app.get("/api/stat", async (c) => {
    const filePath = c.req.query("path");
    if (!filePath) return c.json({ error: "缺少 path 参数" }, 400);

    const fullPath = join(docsDir, filePath);
    if (!fullPath.startsWith(docsDir)) {
      return c.json({ error: "非法路径" }, 403);
    }

    try {
      const info = await stat(fullPath);
      if (info.isDirectory()) return c.json({ error: "不支持目录" }, 400);
      return c.json({ mtime: info.mtimeMs, size: info.size });
    } catch (err: any) {
      return c.json({ error: err.message }, 404);
    }
  });
}
