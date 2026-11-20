import type { Database } from 'bun:sqlite';
import { newPlatformId } from '../ids';
import { nowMillis } from './shared';

export type PlatformSubagentStatus = 'active' | 'closed';

export interface PlatformSubagentRow {
  readonly id: string;
  readonly session_id: string;
  readonly name: string;
  readonly instructions: string | null;
  readonly parent_agent_id: string;
  readonly status: PlatformSubagentStatus;
  readonly opened_at: number;
  readonly closed_at: number | null;
}

export interface NewPlatformSubagent {
  readonly id?: string;
  readonly sessionId: string;
  readonly name: string;
  readonly instructions?: string | null;
  readonly parentAgentId: string;
  readonly status?: PlatformSubagentStatus;
  /** Milliseconds; the projection's start time when the caller observed one. */
  readonly openedAt?: number;
  /** Required to insert an already-closed row adopted from history. */
  readonly closedAt?: number | null;
}

const SUBAGENT_COLUMNS = 'id, session_id, name, instructions, parent_agent_id, status, opened_at, closed_at';

export interface PlatformSubagentsRepositoryOptions {
  readonly now?: () => number;
}

export class PlatformSubagentsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformSubagentsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  get(id: string): PlatformSubagentRow | null {
    const row = this.db
      .query<PlatformSubagentRow, [string]>(`SELECT ${SUBAGENT_COLUMNS} FROM platform_subagents WHERE id = ?`)
      .get(id);
    return row ?? null;
  }

  listBySession(sessionId: string): PlatformSubagentRow[] {
    return this.db
      .query<PlatformSubagentRow, [string]>(
        `SELECT ${SUBAGENT_COLUMNS} FROM platform_subagents
         WHERE session_id = ? ORDER BY opened_at ASC, id ASC`,
      )
      .all(sessionId);
  }

  create(input: NewPlatformSubagent): PlatformSubagentRow {
    const id = input.id ?? newPlatformId();
    const status = input.status ?? 'active';
    const openedAt = input.openedAt ?? nowMillis(this.#now);
    const closedAt = input.closedAt ?? (status === 'closed' ? openedAt : null);
    this.db
      .query(
        `INSERT INTO platform_subagents (
          id, session_id, name, instructions, parent_agent_id, status, opened_at, closed_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.sessionId,
        input.name,
        input.instructions ?? null,
        input.parentAgentId,
        status,
        openedAt,
        closedAt,
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created subagent ${id} could not be read back`);
    return row;
  }

  close(id: string, closedAt: number = nowMillis(this.#now)): PlatformSubagentRow | null {
    this.db
      .query(`UPDATE platform_subagents SET status = 'closed', closed_at = ? WHERE id = ?`)
      .run(closedAt, id);
    return this.get(id);
  }
}
