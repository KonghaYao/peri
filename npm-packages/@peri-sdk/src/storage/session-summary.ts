import type { ChatSessionDirectoryEntry } from "../contracts/chat";

export const SESSION_LIST_SQL = `
  SELECT id, title, cwd, message_count, created_at, updated_at
  FROM threads
  WHERE hidden = 0 AND cwd = ?
  ORDER BY updated_at DESC, id DESC
`;

export const SESSION_BY_ID_SQL = `
  SELECT id, title, cwd, message_count, created_at, updated_at
  FROM threads
  WHERE id = ?
`;

export type SessionRow = {
  id: string;
  title: string | null;
  cwd: string;
  message_count: number;
  created_at: string;
  updated_at: string;
};

/** 存储行 → SDK 只读目录条目；`title` 保持可空，不把缺失标题伪造成“New chat”。 */
export function sessionDirectoryEntry(
  row: SessionRow,
): ChatSessionDirectoryEntry {
  return {
    sessionId: row.id,
    title: row.title,
    cwd: row.cwd,
    messageCount: row.message_count,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
  };
}
