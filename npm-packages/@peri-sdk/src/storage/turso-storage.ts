import { connect } from "@tursodatabase/serverless";
import type { ChatDatabase, ChatSessionDirectoryEntry } from "../contracts/chat";
import {
  SESSION_BY_ID_SQL,
  SESSION_LIST_SQL,
  sessionDirectoryEntry,
  type SessionRow,
} from "./session-summary";

/**
 * libSQL 只读会话目录：直接实现 SDK `ChatDatabase`。
 *
 * 显式 `path` 只查该 cwd；省略 `path` 时依次读取历史 `legacyCwd` 与当前
 * `currentCwd` 并去重。`getSession(id)` 只按 id 读取，不追加 cwd 条件。
 * 缺表错误收敛为空目录，其它存储错误照常抛出。
 */
export class TursoStorage implements ChatDatabase {
  constructor(
    private readonly options: {
      url: string;
      authToken?: string;
      /** `listSessions()` 省略 path 时的历史 cwd；默认 `/workspace`。 */
      legacyCwd?: string;
      /** `listSessions()` 省略 path 时的当前 cwd；默认 `/workspace/`。 */
      currentCwd?: string;
    },
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
    // 串行读取：并发连接各自的 close 会被 peri-loom 以 quota_exceeded（429）拒绝。
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

  private async querySessions(cwd: string): Promise<ChatSessionDirectoryEntry[]> {
    const rows = await this.query(SESSION_LIST_SQL, cwd);
    return rows.map(sessionDirectoryEntry);
  }

  private async query(sql: string, param: string): Promise<SessionRow[]> {
    const connection = connect({
      url: this.options.url,
      authToken: this.options.authToken,
    });
    try {
      try {
        return (await connection.all(sql, param)) as SessionRow[];
      } catch (error) {
        // 新库尚未建表；loom 的 Hrana 把缺表报成 prepare 失败。
        if (isMissingSessionSchema(error)) return [];
        throw error;
      }
    } finally {
      await connection.close();
    }
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
