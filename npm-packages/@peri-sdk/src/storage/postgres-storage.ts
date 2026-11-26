import type { ChatDatabase, ChatSessionDirectoryEntry } from "../contracts/chat";
import {
  SESSION_BY_ID_SQL,
  SESSION_LIST_SQL,
  sessionDirectoryEntry,
  type SessionRow,
} from "./session-summary";

export const QUERY_TIMEOUT_MS = 20_000;

// Pin lookup to the first schema. A missing tenant table must never fall through
// to another schema on search_path. Preserve shorter caller or role timeouts.
export const SET_DIRECTORY_SCOPE = `
  SELECT pg_catalog.set_config('search_path', pg_catalog.quote_ident(pg_catalog.current_schema()), true),
         pg_catalog.set_config('statement_timeout',
           CASE WHEN setting::bigint BETWEEN 1 AND ${QUERY_TIMEOUT_MS} THEN setting ELSE '${QUERY_TIMEOUT_MS}' END, true)
  FROM pg_catalog.pg_settings WHERE name = 'statement_timeout'
`;

export interface PostgresOptions {
  url: string;
  /** Trusted Neon SQL-over-HTTP endpoint for a self-hosted database. */
  httpEndpoint?: string;
  legacyCwd?: string;
  currentCwd?: string;
}

export interface DirectoryConnection {
  all(query: string, param: string): Promise<SessionRow[]>;
  close(): Promise<void>;
}

type OpenConnection = (options: PostgresOptions) => DirectoryConnection;

function postgresUrl(dsn: string): URL {
  let url: URL;
  try { url = new URL(dsn); }
  catch { throw new Error("Invalid PostgreSQL connection URL"); }
  if (!["postgres:", "postgresql:"].includes(url.protocol) || !url.hostname)
    throw new Error("Expected a PostgreSQL connection URL");
  return url;
}

/** A Worker or browser reaches PostgreSQL via its trusted SQL-over-HTTP endpoint. */
export const openHttp: OpenConnection = ({ url, httpEndpoint }) => {
  const host = postgresUrl(url).hostname;
  if (!httpEndpoint && !host.endsWith(".neon.tech"))
    throw new Error("PostgreSQL HTTP transport requires httpEndpoint for this host");
  const endpoint = new URL(httpEndpoint ?? `https://${host.replace(/^[^.]+\./, "api.")}/sql`);
  const local = ["localhost", "127.0.0.1", "[::1]"].includes(endpoint.hostname);
  if (endpoint.username || endpoint.password || endpoint.search || endpoint.hash ||
      !(endpoint.protocol === "https:" || (endpoint.protocol === "http:" && local)))
    throw new Error("PostgreSQL httpEndpoint requires HTTPS or loopback HTTP");
  return {
    async all(query, param) {
      const response = await fetch(endpoint, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Neon-Connection-String": url,
          "Neon-Batch-Read-Only": "true",
        },
        body: JSON.stringify({ queries: [
          { query: SET_DIRECTORY_SCOPE, params: [] },
          { query, params: [param] },
        ] }),
        redirect: "error",
        signal: AbortSignal.timeout(QUERY_TIMEOUT_MS),
      });
      if (!response.ok) {
        const body = await response.json().catch(() => ({}));
        throw Object.assign(new Error(`PostgreSQL HTTP query failed (${response.status})`),
          { code: body?.code });
      }
      const body = await response.json();
      if (body.results?.length !== 2 || !Array.isArray(body.results[1]?.rows))
        throw new Error("Invalid PostgreSQL HTTP result");
      return body.results[1].rows;
    },
    async close() {},
  };
};

/** PostgreSQL session directory. Reads and close share one queue across the instance. */
export class PostgresStorage implements ChatDatabase {
  private connection: DirectoryConnection | null = null;
  private tail: Promise<void> = Promise.resolve();
  private closed = false;

  constructor(
    private readonly options: PostgresOptions,
    private readonly open: OpenConnection = openHttp,
  ) {
    postgresUrl(options.url);
  }

  /**
   * Peri 写侧启动配置：只含连接字段，可直接作为 `store.storage`。
   * 目录读取的 legacyCwd/currentCwd 不属于 Peri 启动对象，故不包含。
   */
  get storageConfig(): { readonly url: string; readonly httpEndpoint?: string } {
    const { url, httpEndpoint } = this.options;
    return httpEndpoint === undefined ? { url } : { url, httpEndpoint };
  }

  async listSessions(path?: string): Promise<ChatSessionDirectoryEntry[]> {
    if (path !== undefined) return this.querySessions(path);
    const legacy = await this.querySessions(
      this.options.legacyCwd ?? "/workspace",
    );
    const current = await this.querySessions(
      this.options.currentCwd ?? "/workspace/",
    );
    return mergeSessionDirectories(legacy, current);
  }

  async getSession(id: string): Promise<ChatSessionDirectoryEntry | null> {
    const rows = await this.query(SESSION_BY_ID_SQL, id);
    return rows[0] ? sessionDirectoryEntry(rows[0]) : null;
  }

  /**
   * 关闭实例持有的连接。幂等：成功后重复调用不再发 close；在途查询完成后才关闭。
   * close 失败时保留连接，后续调用可重试；`closed` 已置位，新查询仍会被拒绝。
   * `ChatDatabase` 契约本身没有关闭动作，长生命周期宿主应在丢弃实例前调用一次。
   */
  close(): Promise<void> {
    return this.serialize(async () => {
      this.closed = true;
      const connection = this.connection;
      // 失败时不清空：下一次 close() 仍能拿到同一条连接重试。
      await connection?.close();
      this.connection = null;
    });
  }

  private async querySessions(cwd: string): Promise<ChatSessionDirectoryEntry[]> {
    const rows = await this.query(SESSION_LIST_SQL, cwd);
    return rows.map(sessionDirectoryEntry);
  }

  private query(sql: string, param: string): Promise<SessionRow[]> {
    return this.serialize(async () => {
      if (this.closed) throw new Error("PostgresStorage is closed");
      const connection = (this.connection ??= this.open({
        url: this.options.url,
        httpEndpoint: this.options.httpEndpoint,
      }));
      try {
        return await connection.all(sql, param);
      } catch (error) {
        // An uninitialized database has no threads relation (undefined_table).
        if (isMissingSessionSchema(error)) return [];
        throw error;
      }
    });
  }

  /** 所有连接动作共用一条队列：前一个动作（含失败）结束后才轮到下一个。 */
  private serialize<T>(run: () => Promise<T>): Promise<T> {
    const task = this.tail.then(run);
    this.tail = task.then(
      () => undefined,
      () => undefined,
    );
    return task;
  }
}

/** 合并旧/新 cwd 目录；同 id 以当前 cwd 为准，按更新时间倒序。 */
export function mergeSessionDirectories(
  legacy: readonly ChatSessionDirectoryEntry[],
  current: readonly ChatSessionDirectoryEntry[],
): ChatSessionDirectoryEntry[] {
  const byId = new Map<string, ChatSessionDirectoryEntry>();
  for (const entry of legacy) byId.set(entry.sessionId, entry);
  for (const entry of current) byId.set(entry.sessionId, entry);
  return [...byId.values()].sort(
    (left, right) =>
      right.updatedAt.localeCompare(left.updatedAt) ||
      right.sessionId.localeCompare(left.sessionId),
  );
}

function isMissingSessionSchema(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === "42P01";
}
