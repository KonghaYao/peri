import { Hono } from "hono";
import { extname, join } from "path";
import { readFile, stat } from "fs/promises";
import type { ViewerDataSource, MemCacheLike } from "./routes/api.js";
import { registerApiRoutes } from "./routes/api.js";

export class MemCache implements MemCacheLike {
  private readonly store = new Map<string, { data: unknown; expiresAt: number }>();
  private cleanupTimer: ReturnType<typeof setInterval> | undefined;

  get<T>(key: string): T | null {
    const entry = this.store.get(key);
    if (!entry) return null;
    if (Date.now() >= entry.expiresAt) {
      this.store.delete(key);
      return null;
    }
    return entry.data as T;
  }

  set<T>(key: string, data: T, ttlMs: number): void {
    this.store.set(key, { data, expiresAt: Date.now() + ttlMs });
  }

  startCleanup(intervalMs = 120_000): void {
    this.cleanupTimer = setInterval(() => {
      const now = Date.now();
      for (const [key, value] of this.store) if (now >= value.expiresAt) this.store.delete(key);
    }, intervalMs);
  }

  stopCleanup(): void {
    if (this.cleanupTimer) clearInterval(this.cleanupTimer);
    this.cleanupTimer = undefined;
  }
}

export interface CreateAppOptions {
  publicDir?: string;
  cache?: MemCacheLike;
}

const MIME_TYPES: Record<string, string> = {
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

export function createApp(dataSource: ViewerDataSource, options: CreateAppOptions = {}): Hono {
  const app = new Hono();
  const cache = options.cache ?? new MemCache();
  registerApiRoutes(app, dataSource, cache);
  const publicDir = options.publicDir ?? join(import.meta.dir, "public");

  app.get("/*", async (c) => {
    const requestPath = c.req.path === "/" ? "/index.html" : c.req.path;
    if (requestPath.includes("..")) return c.text("Forbidden", 403);
    const filePath = join(publicDir, requestPath);
    try {
      const fileStat = await stat(filePath);
      if (!fileStat.isFile()) return c.notFound();
      const content = await readFile(filePath);
      return c.body(content, 200, { "Content-Type": MIME_TYPES[extname(filePath).toLowerCase()] ?? "application/octet-stream" });
    } catch {
      return c.notFound();
    }
  });

  return app;
}
