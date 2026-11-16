import { LocalSessionIndex } from "./ownership";
import type { ChatSessionIndex } from "./service-context";

/**
 * 单个句柄的本地 Session 索引（reserve/commit/release/workspace 归属）。
 *
 * 唯一实现：`ChatManager` 与 `createChatSession` 共用同一份登记语义，差异只在
 * 生命周期——Manager 的索引跨 Chat 共享，工厂的索引随单次调用建立并被句柄独占。
 */
export function createChatSessionIndex(
    sessions: LocalSessionIndex = new LocalSessionIndex(),
): ChatSessionIndex {
    const workspaces = new Map<string, string>();
    return {
        reserve: (chatId, sessionId) => sessions.reserve(sessionId, chatId),
        commit: (chatId, previousSessionId, sessionId) =>
            sessions.commit(chatId, previousSessionId, sessionId),
        release: (chatId, sessionId) => sessions.release(sessionId, chatId),
        workspaceOf: (sessionId) => workspaces.get(sessionId),
        claimWorkspace: (sessionId, workspaceId) => {
            // 首次记录即归属；同一 Session 不允许在同一索引内改写。
            if (!workspaces.has(sessionId)) {
                workspaces.set(sessionId, workspaceId);
            }
        },
    };
}
