import type {
  ChatDatabase,
  ChatSessionDirectoryEntry,
  SessionStorage,
  SessionSummary,
} from "@peri-code/sdk";
import { TursoStorage } from "@peri-code/sdk";
import { SESSION_CWD, WORKSPACE_CWD, type ServerConfig } from "./config";

/**
 * 服务端只读会话目录：数据库事实 → `ChatDatabase`。
 *
 * 不合并运行态、不伪造标题、不在查询失败时返回空列表。列表是目录事实；
 * 运行态只能来自当前 Manager 的句柄。
 */

/** 存储摘要 → 目录条目；`title` 保持可空，不把缺失标题伪造成“New chat”。 */
function directoryEntry(summary: SessionSummary): ChatSessionDirectoryEntry {
  return {
    sessionId: summary.id,
    title: summary.title,
    cwd: summary.cwd,
    messageCount: summary.messageCount,
    createdAt: summary.createdAt,
    updatedAt: summary.updatedAt,
  };
}

/**
 * 新库还没有 canonical schema：远端要到首个 session 打开时才建表，而首屏先直接读
 * `threads`。SDK 只把 `no such table: threads` 当空库；loom 的 Hrana 把缺表统一报成
 * `statement could not be prepared (SQL syntax, schema or statement shape)`，于是首屏
 * 变成存储故障。这里只把这条固定查询的缺表错误收敛为空库，其它错误照常抛出。
 */
function isMissingSessionSchema(error: unknown): boolean {
  return error instanceof Error && error.message.includes("statement could not be prepared");
}

/**
 * `getSessions`/`getSession` 的查询固定且已测试，prepare 失败在空库上只可能是缺表。
 * 其它网络/权限/配额错误必须向上抛出，由目录接口标记不可用。
 */
export function emptyStoreTolerant(
  storage: Pick<SessionStorage, "getSessions" | "getSession">,
): Pick<SessionStorage, "getSessions" | "getSession"> {
  return {
    async getSessions(cwd) {
      try {
        return await storage.getSessions(cwd);
      } catch (error) {
        if (isMissingSessionSchema(error)) return [];
        throw error;
      }
    },
    async getSession(id) {
      try {
        return await storage.getSession(id);
      } catch (error) {
        if (isMissingSessionSchema(error)) return null;
        throw error;
      }
    },
  };
}

/** 按 id 合并旧/新 cwd 的目录；同 id 以当前 cwd 的条目为准。 */
function mergeDirectory(
  legacy: readonly SessionSummary[],
  current: readonly SessionSummary[],
): ChatSessionDirectoryEntry[] {
  const byId = new Map<string, SessionSummary>();
  for (const summary of legacy) byId.set(summary.id, summary);
  // 当前 cwd 是修复后的 canonical 路径；重复行以它为准。
  for (const summary of current) byId.set(summary.id, summary);
  return [...byId.values()]
    .sort((left, right) =>
      right.updatedAt.localeCompare(left.updatedAt)
      || right.id.localeCompare(left.id))
    .map(directoryEntry);
}

/**
 * Turso/libSQL 只读适配器：只回答目录与按 id 的身份。
 *
 * 省略 `path` 时同时读取历史 `/workspace` 与当前 `/workspace/`；显式 `path` 时
 * 严格只查该 cwd。按 id 读取不追加 cwd 认领条件：执行准入与目录范围是两件事。
 */
export class TursoChatDatabase implements ChatDatabase {
  constructor(
    private readonly storage: Pick<SessionStorage, "getSessions" | "getSession">,
  ) {}

  async listSessions(path?: string): Promise<ChatSessionDirectoryEntry[]> {
    if (path !== undefined) {
      return (await this.storage.getSessions(path)).map(directoryEntry);
    }
    // 串行读取：并发连接各自的 close 会被 peri-loom 以 quota_exceeded（429）拒绝。
    // 移除条件：loom 修复并发 close 的配额问题后。
    const legacy = await this.storage.getSessions(WORKSPACE_CWD);
    const current = await this.storage.getSessions(SESSION_CWD);
    return mergeDirectory(legacy, current);
  }

  async getSession(id: string): Promise<ChatSessionDirectoryEntry | null> {
    const summary = await this.storage.getSession(id);
    return summary ? directoryEntry(summary) : null;
  }
}

/** 生产装配：缺表容忍只包裹固定目录查询，其它错误继续上抛。 */
export function createChatDatabase(config: ServerConfig): ChatDatabase {
  const storage = emptyStoreTolerant(new TursoStorage({
    url: config.storage.url,
    authToken: config.storage.authToken,
  }));
  return new TursoChatDatabase(storage);
}
