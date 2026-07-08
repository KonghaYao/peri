import { Hono } from "hono";
import { join, extname, dirname } from "path";
import { stat, readFile } from "fs/promises";
import { fileURLToPath } from "url";
import { DataLoader } from "./data/loader.js";
import { registerApiRoutes } from "./routes/api.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PORT = 8741;
const PUBLIC_DIR = join(__dirname, "public");

const dl = new DataLoader();

// ── 简单内存缓存 ──
interface CacheEntry<T = unknown> {
  data: T;
  expiresAt: number;
}

class MemCache {
  private store = new Map<string, CacheEntry>();

  get<T>(key: string): T | null {
    const entry = this.store.get(key);
    if (!entry) return null;
    if (Date.now() > entry.expiresAt) {
      this.store.delete(key);
      return null;
    }
    return entry.data as T;
  }

  set<T>(key: string, data: T, ttlMs: number): void {
    this.store.set(key, { data, expiresAt: Date.now() + ttlMs });
  }

  /** 定期清理过期条目 */
  private cleanupInterval: ReturnType<typeof setInterval> | null = null;

  startCleanup(intervalMs = 120_000) {
    this.cleanupInterval = setInterval(() => {
      const now = Date.now();
      for (const [k, v] of this.store) {
        if (now > v.expiresAt) this.store.delete(k);
      }
    }, intervalMs);
  }

  stopCleanup() {
    if (this.cleanupInterval) clearInterval(this.cleanupInterval);
  }
}

const cache = new MemCache();
cache.startCleanup();

const app = new Hono();

// API 路由
registerApiRoutes(app, dl, cache);

// 静态文件 catch-all（必须最后注册）
app.get("/*", async (c) => {
  const reqPath = c.req.path === "/" ? "/index.html" : c.req.path;

  // 路径穿越防护
  if (reqPath.includes("..")) {
    return c.text("Forbidden", 403);
  }

  const filePath = join(PUBLIC_DIR, reqPath);
  try {
    const statInfo = await stat(filePath);
    if (statInfo.isFile()) {
      const content = await readFile(filePath);
      const ext = extname(filePath).toLowerCase();
      const mimeMap: Record<string, string> = {
        ".html": "text/html; charset=utf-8",
        ".css": "text/css; charset=utf-8",
        ".js": "application/javascript; charset=utf-8",
        ".mjs": "application/javascript; charset=utf-8",
        ".json": "application/json",
        ".png": "image/png",
        ".jpg": "image/jpeg",
        ".jpeg": "image/jpeg",
        ".svg": "image/svg+xml",
        ".ico": "image/x-icon",
        ".woff2": "font/woff2",
        ".map": "application/json",
      };
      return c.body(content, 200, {
        "Content-Type": mimeMap[ext] || "application/octet-stream",
      });
    }
  } catch {}
  return c.notFound();
});

Bun.serve({ fetch: app.fetch, port: PORT });

console.log(`Peri DB Viewer running at http://localhost:${PORT}`);
