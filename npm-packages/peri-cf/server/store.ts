import type { SessionStorage, SessionSummary } from "@peri-code/sdk/portable";
import type { Chat, ChatSessionStore, ChatSummary } from "../core/agent";
import { SESSION_CWD, WORKSPACE_CWD } from "./config";

/** 存储层事实：`messageCount` 来自 `threads` 表既有列，不新增表或列。 */
function summaryFrom(summary: SessionSummary): ChatSummary {
  return {
    id: summary.id, title: summary.title ?? "New chat", updatedAt: summary.updatedAt,
    messageCount: summary.messageCount,
    running: false,
    pendingInteractionCount: 0,
    lastTurnStatus: null,
    lastCompletedAt: null,
  };
}

/** 基础身份：选中会话、创建响应与 `SyncState.chat` 只使用这三个字段。 */
function chatFrom(summary: SessionSummary): Chat {
  return { id: summary.id, title: summary.title ?? "New chat", updatedAt: summary.updatedAt };
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

/** `getSessions`/`getSession` 的查询固定且已测试，prepare 失败在空库上只可能是缺表。 */
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

/**
 * Turso/libSQL 的只读会话适配器：`list` 与 `get` 之外不承担创建或运行期职责。
 * 创建会话由 `ChatService` 通过 `SessionCreator` 编排。
 */
export class TursoChatSessionStore implements ChatSessionStore {
  /** 注入只读的 SDK 存储；缺表容忍由组合根负责包装。 */
  constructor(private readonly storage: Pick<SessionStorage, "getSessions" | "getSession">) {}

  /** 列出旧/新 cwd 的会话，按 id 去重后按更新时间倒序。 */
  async list(): Promise<ChatSummary[]> {
    // 旧会话落库的 cwd 是 `/workspace`，修复后新建的是 `/workspace/`（见 SESSION_CWD）；
    // 两种都要能列出，按 id 去重后保持与 SQL 相同的排序。
    // 串行读取：并发连接各自的 close 会被 peri-loom 以 quota_exceeded（429）拒绝。
    // 移除条件：loom 修复并发 close 的配额问题后。
    const legacy = await this.storage.getSessions(WORKSPACE_CWD);
    const current = await this.storage.getSessions(SESSION_CWD);
    const byId = new Map(legacy.concat(current).map((summary) => [summary.id, summaryFrom(summary)]));
    return [...byId.values()].sort((left, right) =>
      right.updatedAt.localeCompare(left.updatedAt) || right.id.localeCompare(left.id));
  }

  /** 读取会话身份；cwd 不属于本工作区时视为不存在。 */
  async get(id: string): Promise<Chat | null> {
    const summary = await this.storage.getSession(id);
    return summary && (summary.cwd === WORKSPACE_CWD || summary.cwd === SESSION_CWD)
      ? chatFrom(summary) : null;
  }
}
