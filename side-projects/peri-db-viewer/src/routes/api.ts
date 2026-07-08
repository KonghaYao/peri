import type { Hono } from "hono";
import type { DataLoader } from "../data/loader.js";

// MemCache 的轻量接口（server.ts 中定义，此处不重复导出）
interface MemCacheLike {
  get<T>(key: string): T | null;
  set<T>(key: string, data: T, ttlMs: number): void;
}

export function registerApiRoutes(app: Hono, dl: DataLoader, cache: MemCacheLike) {
  // ── Dashboard ──

  /** GET /api/stats — 总览统计 + 状态分布（缓存 30s） */
  app.get("/api/stats", (c) => {
    try {
      const cached = cache.get("/api/stats");
      if (cached) return c.json(cached);

      const stats = dl.getStats();
      const statusDist = dl.getAgentStatusDist();
      const subAgents = dl.loadAllSubAgents();
      const data = {
        ...stats,
        totalSubAgents: subAgents.length,
        agentStatusDist: statusDist,
      };

      cache.set("/api/stats", data, 30_000);
      return c.json(data);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  /** GET /api/timeline?days=30 — 日活跃时间线（缓存 60s） */
  app.get("/api/timeline", (c) => {
    try {
      const days = parseInt(c.req.query("days") ?? "30") || 30;
      const cacheKey = `/api/timeline:${days}`;

      const cached = cache.get(cacheKey);
      if (cached) return c.json(cached);

      const timeline = dl.getTimeline(days);

      cache.set(cacheKey, timeline, 60_000);
      return c.json(timeline);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  // ── Thread List ──

  /** GET /api/cwds — 所有不重复的 cwd 值 */
  app.get("/api/cwds", (c) => {
    try {
      return c.json(dl.getDistinctCwds());
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  /** GET /api/threads?page&perPage&sort&order&status&search&cwd&minMsg */
  app.get("/api/threads", (c) => {
    try {
      const page = parseInt(c.req.query("page") ?? "1") || 1;
      const perPage = parseInt(c.req.query("perPage") ?? "50") || 50;
      const sort = c.req.query("sort") ?? "updated_at";
      const order = c.req.query("order") ?? "DESC";
      const status = c.req.query("status") || undefined;
      const search = c.req.query("search") || undefined;
      const cwd = c.req.query("cwd") || undefined;
      const minMsg = c.req.query("minMsg") ? parseInt(c.req.query("minMsg")!) : undefined;

      const rows = dl.loadThreadsPaginated(page, perPage, sort, order, status, search, cwd, minMsg);
      const total = dl.getThreadCount(status, search, cwd, minMsg);
      // 为每行附加子 agent 数量
      const enriched = rows.map((t) => ({
        ...t,
        subagent_count: dl.loadSubAgents(t.id).length,
      }));
      return c.json({ rows: enriched, total, page, perPage });
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  // ── Detail ──

  /** GET /api/threads/:id — 单条会话详情 */
  app.get("/api/threads/:id", (c) => {
    try {
      const id = c.req.param("id");
      const thread = dl.getThreadById(id);
      if (!thread) return c.json({ error: "Thread not found" }, 404);

      // 父/子关系
      const parent = thread.parent_thread_id
        ? dl.getThreadById(thread.parent_thread_id)
        : null;
      const children = dl.loadSubAgents(id);

      return c.json({ thread, parent, children });
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  /** GET /api/threads/:id/messages — 会话消息列表 */
  app.get("/api/threads/:id/messages", (c) => {
    try {
      const id = c.req.param("id");
      const messages = dl.loadMessages(id);
      return c.json({ messages });
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  // ── Tool Analysis ──

  /** GET /api/tools/stats — 工具统计（缓存 60s，查询昂贵） */
  app.get("/api/tools/stats", (c) => {
    try {
      const cached = cache.get("/api/tools/stats");
      if (cached) return c.json(cached);

      const errorRate = dl.getToolErrorRate();
      const recentErrors = dl.getRecentToolErrors(50);
      const data = { errorRate, recentErrors };

      cache.set("/api/tools/stats", data, 60_000);
      return c.json(data);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  // ── Search ──

  /** GET /api/search?q&thread_id&page&perPage */
  app.get("/api/search", (c) => {
    try {
      const q = c.req.query("q");
      if (!q) return c.json({ error: "Missing query parameter 'q'" }, 400);
      const threadId = c.req.query("thread_id") || undefined;
      const page = parseInt(c.req.query("page") ?? "1") || 1;
      const perPage = parseInt(c.req.query("perPage") ?? "20") || 20;

      const result = dl.searchMessages(q, threadId, page, perPage);
      return c.json(result);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });
}
