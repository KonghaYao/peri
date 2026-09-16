import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import type { NormalizedViewerMessage, ToolErrorRow, ToolStat, ViewerDataSource, ViewerStats, ViewerThread } from "./routes/api.js";

type SharedLoader = {
  new (path: string): {
    capabilities: { columns: Record<string, string[]> };
    loadMessages(threadId: string): Array<any>;
    loadInheritedMessages(threadId: string): Array<any>;
    iterateNormalizedMessages(threadId?: string): IterableIterator<any>;
    close(): void;
  };
};
type SharedModule = { DataLoader: SharedLoader; normalizeMessage(row: any, origin?: "own" | "inherited"): NormalizedViewerMessage };

export const DEFAULT_DB_PATH = join(homedir(), ".peri/threads/threads.db");

function expandPath(path: string): string {
  return path === "~" ? homedir() : path.startsWith("~/") ? join(homedir(), path.slice(2)) : path;
}

function has(columns: Record<string, string[]>, table: string, column: string): boolean {
  return columns[table]?.includes(column) ?? false;
}

function field(columns: Record<string, string[]>, table: string, alias: string, fallback: string): string {
  return has(columns, table, alias) ? `${table[0]}.${alias}` : `${fallback} AS ${alias}`;
}

export class ViewerDataAdapter implements ViewerDataSource {
  private readonly db: Database;
  private readonly shared: { loader: InstanceType<SharedLoader>; normalize: SharedModule["normalizeMessage"] };
  private readonly columns: Record<string, string[]>;

  private constructor(db: Database, shared: ViewerDataAdapter["shared"], columns: Record<string, string[]>) {
    this.db = db;
    this.shared = shared;
    this.columns = columns;
  }

  static async open(dbPath = DEFAULT_DB_PATH): Promise<ViewerDataAdapter> {
    const path = expandPath(dbPath);
    const modulePath = process.env.PERI_ANALYZER_DATA_MODULE ?? join(import.meta.dir, "../../agent-defect-analyzer/src/data/loader.ts");
    const sharedModule = await import(modulePath) as unknown as SharedModule;
    const loader = new sharedModule.DataLoader(path);
    const db = new Database(path, { readonly: true });
    return new ViewerDataAdapter(db, { loader, normalize: sharedModule.normalizeMessage }, loader.capabilities.columns);
  }

  close(): void {
    this.db.close();
    this.shared.loader.close();
  }

  private query<T>(sql: string, ...params: unknown[]): T[] {
    return this.db.query(sql).all(...params as any[]) as T[];
  }

  private one<T>(sql: string, ...params: unknown[]): T | null {
    return (this.db.query(sql).get(...params as any[]) as T | null) ?? null;
  }

  private threadColumns(): string {
    const c = (name: string, fallback: string) => field(this.columns, "threads", name, fallback);
    return ["t.id", "t.title", "t.cwd", "t.created_at", "t.updated_at", "t.message_count",
      c("parent_thread_id", "NULL"), c("snapshot_at_message_id", "NULL"), c("hidden", "0"),
      c("cancel_policy", "NULL"), c("agent_status", "NULL"),
      c("context_cache_epoch", "NULL")].join(", ");
  }

  private messageColumns(): string {
    const c = (name: string, fallback: string) => has(this.columns, "messages", name) ? `m.${name}` : `${fallback} AS ${name}`;
    return ["m.message_id", "m.thread_id", "m.role", "m.content", c("truncated", "0"), c("excluded", "0"), c("projection", "NULL"), "m.rowid AS sequence"].join(", ");
  }

  private messageRows(threadId: string): Array<any> {
    return this.shared.loader.loadMessages(threadId);
  }

  private messageRowsPage(threadId: string, offset: number, limit: number): Array<any> {
    return this.query<any>(`SELECT ${this.messageColumns()} FROM messages m WHERE m.thread_id=? ORDER BY m.rowid LIMIT ? OFFSET ?`, threadId, limit, offset);
  }

  private mainWhere(): string {
    const hidden = has(this.columns, "threads", "hidden") ? "t.hidden=0" : "1=1";
    const root = has(this.columns, "threads", "parent_thread_id") ? "t.parent_thread_id IS NULL" : "1=1";
    return `${hidden} AND ${root}`;
  }

  getStats(): ViewerStats {
    const hidden = has(this.columns, "threads", "hidden") ? "SUM(t.hidden=0)" : "COUNT(*)";
    const t = this.one<any>(`SELECT COUNT(*) total, ${hidden} visible FROM threads t`) ?? { total: 0, visible: 0 };
    const m = this.one<any>("SELECT COUNT(*) total FROM messages") ?? { total: 0 };
    const roles = this.query<any>("SELECT role, COUNT(*) count FROM messages GROUP BY role");
    let errors = 0;
    for (const message of this.shared.loader.iterateNormalizedMessages()) {
      for (const result of message.results) if (result.isError === true) errors++;
    }
    return { totalThreads: Number(t.total), visibleThreads: Number(t.visible), totalMessages: Number(m.total), roleDistribution: Object.fromEntries(roles.map((r) => [r.role, Number(r.count)])), totalToolErrors: errors };
  }

  getAgentStatusDist(): Array<{ agent_status: string; count: number }> {
    if (!has(this.columns, "threads", "agent_status")) return [];
    const visible = has(this.columns, "threads", "hidden") ? "WHERE hidden=0" : "";
    return this.query<any>(`SELECT COALESCE(agent_status, '') agent_status, COUNT(*) count FROM threads ${visible} GROUP BY agent_status ORDER BY count DESC`).map((r) => ({ agent_status: r.agent_status, count: Number(r.count) }));
  }

  loadAllSubAgents(): ViewerThread[] {
    if (!has(this.columns, "threads", "parent_thread_id")) return [];
    return this.query<ViewerThread>(`SELECT ${this.threadColumns()} FROM threads t WHERE t.parent_thread_id IS NOT NULL ORDER BY t.created_at,t.id`);
  }

  getTimeline(days: number): Array<{ date: string; count: number }> {
    const cutoff = new Date(Date.now() - days * 86_400_000).toISOString();
    return this.query<any>(`SELECT DATE(t.created_at) date, COUNT(*) count FROM threads t WHERE ${has(this.columns, "threads", "hidden") ? "t.hidden=0 AND" : ""} t.created_at>=? GROUP BY date ORDER BY date`, cutoff).map((r) => ({ date: r.date, count: Number(r.count) }));
  }

  getDistinctCwds(): string[] {
    return this.query<{ cwd: string }>(`SELECT DISTINCT t.cwd FROM threads t WHERE ${this.mainWhere()} AND t.cwd!='' ORDER BY t.cwd`).map((r) => r.cwd);
  }

  loadThreadsPaginated(page: number, perPage: number, sort: string, order: string, status?: string, search?: string, cwd?: string, minMsg?: number): ViewerThread[] {
    const conditions = [this.mainWhere()];
    const params: unknown[] = [];
    if (status && has(this.columns, "threads", "agent_status")) { conditions.push("t.agent_status=?"); params.push(status); }
    if (search) { conditions.push("(t.title LIKE ? OR t.id LIKE ?)"); params.push(`%${search}%`, `%${search}%`); }
    if (cwd) { conditions.push("t.cwd=?"); params.push(cwd); }
    if (minMsg !== undefined) { conditions.push("t.message_count>=?"); params.push(minMsg); }
    params.push(perPage, (page - 1) * perPage);
    return this.query<ViewerThread>(`SELECT ${this.threadColumns()} FROM threads t WHERE ${conditions.join(" AND ")} ORDER BY t.${sort} ${order === "ASC" ? "ASC" : "DESC"}, t.id LIMIT ? OFFSET ?`, ...params);
  }

  getThreadCount(status?: string, search?: string, cwd?: string, minMsg?: number): number {
    const conditions = [this.mainWhere()];
    const params: unknown[] = [];
    if (status && has(this.columns, "threads", "agent_status")) { conditions.push("t.agent_status=?"); params.push(status); }
    if (search) { conditions.push("(t.title LIKE ? OR t.id LIKE ?)"); params.push(`%${search}%`, `%${search}%`); }
    if (cwd) { conditions.push("t.cwd=?"); params.push(cwd); }
    if (minMsg !== undefined) { conditions.push("t.message_count>=?"); params.push(minMsg); }
    return Number(this.one<any>(`SELECT COUNT(*) count FROM threads t WHERE ${conditions.join(" AND ")}`, ...params)?.count ?? 0);
  }

  loadSubAgents(parentThreadId: string): ViewerThread[] {
    if (!has(this.columns, "threads", "parent_thread_id")) return [];
    return this.query<ViewerThread>(`SELECT ${this.threadColumns()} FROM threads t WHERE t.parent_thread_id=? ORDER BY t.created_at,t.id`, parentThreadId);
  }

  getThreadById(id: string): ViewerThread | null {
    return this.one<ViewerThread>(`SELECT ${this.threadColumns()} FROM threads t WHERE t.id=?`, id);
  }

  loadMessages(threadId: string): NormalizedViewerMessage[] {
    const own = this.messageRows(threadId).map((row) => this.shared.normalize(row, "own"));
    const inherited = this.shared.loader.loadInheritedMessages(threadId) as NormalizedViewerMessage[];
    return [...inherited, ...own];
  }

  getMessageCount(threadId: string): number {
    const own = Number(this.one<any>("SELECT COUNT(*) count FROM messages WHERE thread_id=?", threadId)?.count ?? 0);
    return own + this.shared.loader.loadInheritedMessages(threadId).length;
  }

  loadMessagesPage(threadId: string, offset: number, limit: number): NormalizedViewerMessage[] {
    const inherited = this.shared.loader.loadInheritedMessages(threadId);
    if (offset < inherited.length) {
      const inheritedPage = inherited.slice(offset, offset + limit);
      if (inheritedPage.length >= limit) return inheritedPage;
      const ownRows = this.messageRowsPage(threadId, 0, limit - inheritedPage.length);
      return [...inheritedPage, ...ownRows.map((row) => this.shared.normalize(row, "own"))];
    }
    const ownOffset = offset - inherited.length;
    return this.messageRowsPage(threadId, ownOffset, limit).map((row) => this.shared.normalize(row, "own"));
  }

  getToolStats(): ToolStat[] {
    const calls = new Map<string, { count: number; resultCount: number; knownResultCount: number; unknownResultCount: number; errorCount: number }>();
    const pending = new Map<string, string[]>();
    for (const message of this.shared.loader.iterateNormalizedMessages()) {
      for (const call of message.calls) {
        const key = `${message.threadId}:${call.id}`;
        const queue = pending.get(key) ?? [];
        queue.push(call.name);
        pending.set(key, queue);
        const row = calls.get(call.name) ?? { count: 0, resultCount: 0, knownResultCount: 0, unknownResultCount: 0, errorCount: 0 };
        row.count++;
        calls.set(call.name, row);
      }
      for (const result of message.results) {
        const key = `${message.threadId}:${result.id}`;
        const queue = pending.get(key);
        const name = queue?.shift();
        if (!name) continue;
        if (queue?.length === 0) pending.delete(key);
        const row = calls.get(name)!;
        row.resultCount++;
        if (result.isError === null) row.unknownResultCount++;
        else { row.knownResultCount++; if (result.isError === true) row.errorCount++; }
      }
    }
    return [...calls.entries()].map(([name, row]) => ({ name, ...row, errorRate: row.knownResultCount ? Number((row.errorCount / row.knownResultCount * 100).toFixed(1)) : null })).sort((a, b) => b.count - a.count);
  }

  getRecentToolErrors(limit: number): ToolErrorRow[] {
    const titles = new Map(this.query<{ id: string; title: string | null }>("SELECT id,title FROM threads").map((r) => [r.id, r.title]));
    const errors: ToolErrorRow[] = [];
    const chunkSize = Math.max(100, limit * 2);
    for (let offset = 0; errors.length < limit; offset += chunkSize) {
      const rows = this.query<any>(`SELECT ${this.messageColumns()} FROM messages m ORDER BY m.rowid DESC LIMIT ? OFFSET ?`, chunkSize, offset);
      if (rows.length === 0) break;
      for (const row of rows) {
        const message = this.shared.normalize(row, "own");
        for (const result of message.results) {
          if (result.isError !== true) continue;
          errors.push({ msg_rowid: Number(message.sequence), thread_id: message.threadId, content: result.content, role: "tool", thread_title: titles.get(message.threadId) ?? null });
          if (errors.length >= limit) break;
        }
        if (errors.length >= limit) break;
      }
      if (rows.length < chunkSize) break;
    }
    return errors;
  }

  searchMessages(query: string, threadId: string | undefined, page: number, perPage: number): { rows: any[]; total: number } {
    const conditions = ["m.content LIKE ?"];
    const countParams: unknown[] = [`%${query}%`];
    if (threadId) { conditions.push("m.thread_id=?"); countParams.push(threadId); }
    const where = conditions.join(" AND ");
    const total = Number(this.one<any>(`SELECT COUNT(*) count FROM messages m WHERE ${where}`, ...countParams)?.count ?? 0);
    return { total, rows: this.query<any>(`SELECT m.rowid msg_rowid,m.thread_id,m.role,m.content,t.title thread_title FROM messages m LEFT JOIN threads t ON t.id=m.thread_id WHERE ${where} ORDER BY m.rowid DESC LIMIT ? OFFSET ?`, ...countParams, perPage, (page - 1) * perPage) };
  }
}
