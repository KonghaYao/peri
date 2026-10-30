import { connect, type Connection } from "@tursodatabase/serverless";
import type { ChatDatabase, ChatSessionDirectoryEntry } from "../contracts/chat";
import {
  SESSION_BY_ID_SQL,
  SESSION_LIST_SQL,
  sessionDirectoryEntry,
  type SessionRow,
} from "./session-summary";

/** 连接工厂；生产固定为 SDK `connect`，测试注入假连接观察复用与关闭。 */
type OpenConnection = (config: {
  url: string;
  authToken?: string;
}) => Connection;

/**
 * libSQL 只读会话目录：直接实现 SDK `ChatDatabase`。
 *
 * 显式 `path` 只查该 cwd；省略 `path` 时依次读取历史 `legacyCwd` 与当前
 * `currentCwd` 并去重。`getSession(id)` 只按 id 读取，不追加 cwd 条件。
 * 缺表错误收敛为空目录，其它存储错误照常抛出。
 *
 * 每个实例持有一条连接（首次查询时建立），查询与 `close()` 走同一条串行队列：
 * SDK 连接是单流，并发语句与并发 close 都会竞争服务端 baton（peri-loom 对并发
 * close 回 429），因此不在查询之间反复 connect/close。
 */
export class TursoStorage implements ChatDatabase {
  private connection: Connection | null = null;
  private tail: Promise<void> = Promise.resolve();
  private closed = false;

  constructor(
    private readonly options: {
      url: string;
      authToken?: string;
      /** `listSessions()` 省略 path 时的历史 cwd；默认 `/workspace`。 */
      legacyCwd?: string;
      /** `listSessions()` 省略 path 时的当前 cwd；默认 `/workspace/`。 */
      currentCwd?: string;
    },
    private readonly open: OpenConnection = connect,
  ) {}

  /**
   * Peri 写侧启动配置：只含连接字段，可直接作为 `store.storage`。
   * 目录读取的 legacyCwd/currentCwd 不属于 Peri 启动对象，故不包含。
   */
  get storageConfig(): { readonly url: string; readonly authToken?: string } {
    const { url, authToken } = this.options;
    return authToken === undefined ? { url } : { url, authToken };
  }

  async listSessions(path?: string): Promise<ChatSessionDirectoryEntry[]> {
    if (path !== undefined) return this.querySessions(path);
    // 两次读取顺序执行：同一连接是单流，串行队列也保证不会与 close 竞争 baton。
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
   * 关闭实例持有的连接。幂等：重复调用不会再发 close；在途查询完成后才关闭。
   * `ChatDatabase` 契约本身没有关闭动作，长生命周期宿主应在丢弃实例前调用一次。
   */
  close(): Promise<void> {
    return this.serialize(async () => {
      if (this.closed) return;
      this.closed = true;
      const connection = this.connection;
      this.connection = null;
      await connection?.close();
    });
  }

  private async querySessions(cwd: string): Promise<ChatSessionDirectoryEntry[]> {
    const rows = await this.query(SESSION_LIST_SQL, cwd);
    return rows.map(sessionDirectoryEntry);
  }

  private query(sql: string, param: string): Promise<SessionRow[]> {
    return this.serialize(async () => {
      if (this.closed) throw new Error("TursoStorage is closed");
      const connection = (this.connection ??= this.open({
        url: this.options.url,
        authToken: this.options.authToken,
      }));
      try {
        return (await connection.all(sql, param)) as SessionRow[];
      } catch (error) {
        // 新库尚未建表；loom 的 Hrana 把缺表报成 prepare 失败。
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
  return (
    error instanceof Error &&
    (error.message.includes("no such table: threads") ||
      error.message.includes("statement could not be prepared"))
  );
}
