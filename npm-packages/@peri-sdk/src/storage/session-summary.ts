export interface SessionSummary {
  id: string;
  title: string | null;
  cwd: string;
  messageCount: number;
  createdAt: string;
  updatedAt: string;
}

export const SESSION_LIST_SQL = `
  SELECT id, title, cwd, message_count, created_at, updated_at
  FROM threads
  WHERE hidden = 0 AND cwd = ?
  ORDER BY updated_at DESC, id DESC
`;

export type SessionRow = {
  id: string;
  title: string | null;
  cwd: string;
  message_count: number;
  created_at: string;
  updated_at: string;
};

export function sessionSummary(row: SessionRow): SessionSummary {
  return {
    id: row.id,
    title: row.title,
    cwd: row.cwd,
    messageCount: row.message_count,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
  };
}
