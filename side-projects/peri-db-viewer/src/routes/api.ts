import type { Hono } from "hono";

export interface ViewerThread {
  id: string;
  title: string | null;
  cwd: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  parent_thread_id: string | null;
  snapshot_at_message_id?: string | null;
  hidden: number;
  cancel_policy?: string | null;
  config?: string | null;
  cached_context?: string | null;
  frozen_context?: string | null;
  inherited_context?: string | null;
  agent_status: string | null;
  context_cache_epoch?: number | null;
}

export interface NormalizedViewerMessage {
  messageId: string;
  threadId: string;
  sequence: number;
  origin: "own" | "inherited";
  role: string;
  text: string;
  calls: Array<{ id: string; name: string; arguments: Record<string, unknown>; source: "content" | "tool_calls" }>;
  results: Array<{ id: string; content: string; isError: boolean | null; source: "content" | "message" }>;
  excludedFromContext: boolean;
  truncated: boolean;
  isSummary: boolean;
  parseIssues: string[];
}

export interface ViewerStats {
  totalThreads: number;
  visibleThreads: number;
  totalMessages: number;
  roleDistribution: Record<string, number>;
  totalToolErrors: number;
}

export interface ToolStat {
  name: string;
  count: number;
  resultCount: number;
  errorCount: number;
  knownResultCount: number;
  unknownResultCount: number;
  errorRate: number | null;
}

export interface ToolErrorRow {
  msg_rowid: number;
  thread_id: string;
  content: string;
  role: string;
  thread_title: string | null;
}

export interface ViewerDataSource {
  close(): void;
  getStats(): ViewerStats;
  getAgentStatusDist(): Array<{ agent_status: string; count: number }>;
  loadAllSubAgents(): ViewerThread[];
  getTimeline(days: number): Array<{ date: string; count: number }>;
  getDistinctCwds(): string[];
  loadThreadsPaginated(page: number, perPage: number, sort: string, order: string, status?: string, search?: string, cwd?: string, minMsg?: number): ViewerThread[];
  getThreadCount(status?: string, search?: string, cwd?: string, minMsg?: number): number;
  loadSubAgents(parentThreadId: string): ViewerThread[];
  getThreadById(id: string): ViewerThread | null;
  loadMessages(threadId: string): NormalizedViewerMessage[];
  loadMessagesPage(threadId: string, offset: number, limit: number): NormalizedViewerMessage[];
  getMessageCount(threadId: string): number;
  getToolStats(): ToolStat[];
  getRecentToolErrors(limit: number): ToolErrorRow[];
  searchMessages(query: string, threadId: string | undefined, page: number, perPage: number): { rows: any[]; total: number };
}

export interface MemCacheLike {
  get<T>(key: string): T | null;
  set<T>(key: string, data: T, ttlMs: number): void;
}

const MAX_PAGE = 1_000_000;
const MAX_PER_PAGE = 100;
const MAX_DAYS = 3_650;
const MAX_QUERY_LENGTH = 200;

function badRequest(message: string): Response {
  return Response.json({ error: message }, { status: 400 });
}

function positiveInt(value: string | undefined, fallback: number, name: string, max: number): number | Response {
  if (value === undefined || value === "") return fallback;
  if (!/^\d+$/.test(value)) return badRequest(`${name} must be an integer`);
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > max) return badRequest(`${name} must be between 1 and ${max}`);
  return parsed;
}

function nonNegativeInt(value: string | undefined, name: string): number | undefined | Response {
  if (value === undefined || value === "") return undefined;
  if (!/^\d+$/.test(value)) return badRequest(`${name} must be a non-negative integer`);
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed)) return badRequest(`${name} is too large`);
  return parsed;
}

function cached<T>(cache: MemCacheLike, key: string, load: () => T, ttlMs: number): T {
  const hit = cache.get<T>(key);
  if (hit !== null) return hit;
  const value = load();
  cache.set(key, value, ttlMs);
  return value;
}

export function registerApiRoutes(app: Hono, dl: ViewerDataSource, cache: MemCacheLike): void {
  app.get("/api/stats", (c) => {
    try {
      const data = cached(cache, "/api/stats", () => ({
        ...dl.getStats(),
        totalSubAgents: dl.loadAllSubAgents().length,
        agentStatusDist: dl.getAgentStatusDist(),
      }), 30_000);
      return c.json(data);
    } catch (err: any) {
      return c.json({ error: String(err?.message ?? err) }, 500);
    }
  });

  app.get("/api/timeline", (c) => {
    const days = positiveInt(c.req.query("days"), 30, "days", MAX_DAYS);
    if (days instanceof Response) return days;
    try {
      return c.json(cached(cache, `/api/timeline:${days}`, () => dl.getTimeline(days), 60_000));
    } catch (err: any) {
      return c.json({ error: String(err?.message ?? err) }, 500);
    }
  });

  app.get("/api/cwds", (c) => {
    try { return c.json(dl.getDistinctCwds()); }
    catch (err: any) { return c.json({ error: String(err?.message ?? err) }, 500); }
  });

  app.get("/api/threads", (c) => {
    const page = positiveInt(c.req.query("page"), 1, "page", MAX_PAGE);
    const perPage = positiveInt(c.req.query("perPage"), 50, "perPage", MAX_PER_PAGE);
    const minMsg = nonNegativeInt(c.req.query("minMsg"), "minMsg");
    if (page instanceof Response) return page;
    if (perPage instanceof Response) return perPage;
    if (minMsg instanceof Response) return minMsg;
    const sort = c.req.query("sort") ?? "updated_at";
    const order = c.req.query("order") ?? "DESC";
    if (!["created_at", "updated_at", "message_count"].includes(sort)) return badRequest("sort is not supported");
    if (!["ASC", "DESC"].includes(order.toUpperCase())) return badRequest("order must be ASC or DESC");
    const search = c.req.query("search") || undefined;
    const cwd = c.req.query("cwd") || undefined;
    const status = c.req.query("status") || undefined;
    if (search && search.length > MAX_QUERY_LENGTH) return badRequest(`search must be at most ${MAX_QUERY_LENGTH} characters`);
    try {
      const rows = dl.loadThreadsPaginated(page, perPage, sort, order, status, search, cwd, minMsg);
      const total = dl.getThreadCount(status, search, cwd, minMsg);
      return c.json({ rows: rows.map((thread) => ({ ...thread, subagent_count: dl.loadSubAgents(thread.id).length })), total, page, perPage });
    } catch (err: any) {
      return c.json({ error: String(err?.message ?? err) }, 500);
    }
  });

  app.get("/api/threads/:id", (c) => {
    try {
      const id = c.req.param("id");
      const thread = dl.getThreadById(id);
      if (!thread) return c.json({ error: "Thread not found" }, 404);
      return c.json({
        thread,
        parent: thread.parent_thread_id ? dl.getThreadById(thread.parent_thread_id) : null,
        children: dl.loadSubAgents(id),
      });
    } catch (err: any) {
      return c.json({ error: String(err?.message ?? err) }, 500);
    }
  });

  app.get("/api/threads/:id/messages", (c) => {
    try {
      const id = c.req.param("id");
      if (!dl.getThreadById(id)) return c.json({ error: "Thread not found" }, 404);
      const page = positiveInt(c.req.query("page"), 1, "page", MAX_PAGE);
      const perPage = positiveInt(c.req.query("perPage"), 100, "perPage", MAX_PER_PAGE * 5);
      if (page instanceof Response) return page;
      if (perPage instanceof Response) return perPage;
      const offset = (page - 1) * perPage;
      const total = dl.getMessageCount(id);
      const messages = dl.loadMessagesPage(id, offset, perPage);
      return c.json({ messages, total, offset, perPage, hasMore: offset + messages.length < total });
    } catch (err: any) {
      return c.json({ error: String(err?.message ?? err) }, 500);
    }
  });

  app.get("/api/tools/stats", (c) => {
    try {
      return c.json(cached(cache, "/api/tools/stats", () => ({
        errorRate: dl.getToolStats(),
        recentErrors: dl.getRecentToolErrors(50),
      }), 60_000));
    } catch (err: any) {
      return c.json({ error: String(err?.message ?? err) }, 500);
    }
  });

  app.get("/api/search", (c) => {
    const query = c.req.query("q");
    if (!query) return badRequest("Missing query parameter 'q'");
    if (query.length > MAX_QUERY_LENGTH) return badRequest(`q must be at most ${MAX_QUERY_LENGTH} characters`);
    const page = positiveInt(c.req.query("page"), 1, "page", MAX_PAGE);
    const perPage = positiveInt(c.req.query("perPage"), 20, "perPage", MAX_PER_PAGE);
    if (page instanceof Response) return page;
    if (perPage instanceof Response) return perPage;
    try { return c.json(dl.searchMessages(query, c.req.query("thread_id") || undefined, page, perPage)); }
    catch (err: any) { return c.json({ error: String(err?.message ?? err) }, 500); }
  });
}
