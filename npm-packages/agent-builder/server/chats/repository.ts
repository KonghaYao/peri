import type { Database } from 'bun:sqlite';
import type { Agent } from '@shared/agent';
import type { ChatSummary as ChatSummaryShape } from '@shared/chat';
import { AgentsRepository } from '../agents/repository';

interface ChatRow {
  readonly chat_id: string;
  readonly agent_id: string;
  readonly session_id: string | null;
  readonly created_at: number;
  readonly updated_at: number;
  readonly agent_name: string;
}

export interface ChatWithAgent {
  readonly chat: ChatSummaryShape;
  readonly agent: Agent;
}

export interface ChatsRepositoryOptions {
  readonly now?: () => number;
}

function toSummary(row: ChatRow): ChatSummaryShape {
  return {
    chatId: row.chat_id,
    agentId: row.agent_id,
    agentName: row.agent_name,
    sessionId: row.session_id,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    // Runtime state is owned by ChatService; the database only stores the mapping.
    sandbox: { state: 'disabled', reason: null },
  };
}

const SELECT = `
  SELECT c.chat_id, c.agent_id, c.session_id, c.created_at, c.updated_at, a.name AS agent_name
  FROM chats c
  JOIN agents a ON a.id = c.agent_id
`;

export class ChatsRepository {
  readonly #db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: ChatsRepositoryOptions = {}) {
    this.#db = db;
    this.#now = options.now ?? Date.now;
  }

  list(agentId?: string): ChatSummaryShape[] {
    const rows = agentId
      ? this.#db
          .query<ChatRow, [string]>(
            `${SELECT} WHERE c.agent_id = ? ORDER BY c.updated_at DESC, c.chat_id ASC`,
          )
          .all(agentId)
      : this.#db.query<ChatRow, []>(`${SELECT} ORDER BY c.updated_at DESC, c.chat_id ASC`).all();
    return rows.map(toSummary);
  }

  get(chatId: string): ChatSummaryShape | null {
    const row = this.#db.query<ChatRow, [string]>(`${SELECT} WHERE c.chat_id = ?`).get(chatId);
    return row ? toSummary(row) : null;
  }

  /** 仅首次运行解析时读取；已开启的 Chat 使用 ChatService 的内存冻结目标。 */
  getWithAgent(chatId: string): ChatWithAgent | null {
    const chat = this.get(chatId);
    if (!chat) return null;
    const agent = new AgentsRepository(this.#db).get(chat.agentId);
    if (!agent) throw new Error(`Agent ${chat.agentId} disappeared`);
    return { chat, agent };
  }

  create(chatId: string, agentId: string): ChatSummaryShape {
    const now = this.#now();
    this.#db
      .query(
        `INSERT INTO chats (chat_id, agent_id, session_id, created_at, updated_at)
         VALUES (?, ?, NULL, ?, ?)`,
      )
      .run(chatId, agentId, now, now);

    const created = this.get(chatId);
    if (!created) throw new Error(`Chat ${chatId} disappeared after insert`);
    return created;
  }

  setSessionId(chatId: string, sessionId: string): void {
    this.#db
      .query('UPDATE chats SET session_id = ?, updated_at = ? WHERE chat_id = ?')
      .run(sessionId, this.#now(), chatId);
  }

  touch(chatId: string): void {
    this.#db.query('UPDATE chats SET updated_at = ? WHERE chat_id = ?').run(this.#now(), chatId);
  }

  delete(chatId: string): boolean {
    const result = this.#db.query('DELETE FROM chats WHERE chat_id = ?').run(chatId);
    return result.changes > 0;
  }
}
