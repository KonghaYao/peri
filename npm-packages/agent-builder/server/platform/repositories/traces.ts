import type { Database } from 'bun:sqlite';
import { newPlatformId } from '../ids';
import { nowMillis } from './shared';

export interface PlatformTraceRow {
  readonly id: string;
  readonly session_id: string;
  readonly turn_id: string | null;
  readonly kind: string;
  readonly data: string;
  readonly created_at: number;
}

export interface NewPlatformTrace {
  readonly id?: string;
  readonly sessionId: string;
  readonly turnId?: string | null;
  readonly kind: string;
  readonly data: string;
}

const TRACE_COLUMNS = 'id, session_id, turn_id, kind, data, created_at';

export interface PlatformTracesRepositoryOptions {
  readonly now?: () => number;
}

export class PlatformTracesRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformTracesRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  listBySession(sessionId: string): PlatformTraceRow[] {
    return this.db
      .query<PlatformTraceRow, [string]>(
        `SELECT ${TRACE_COLUMNS} FROM platform_traces
         WHERE session_id = ? ORDER BY created_at ASC, id ASC`,
      )
      .all(sessionId);
  }

  create(input: NewPlatformTrace): PlatformTraceRow {
    const id = input.id ?? newPlatformId();
    this.db
      .query(
        `INSERT INTO platform_traces (id, session_id, turn_id, kind, data, created_at)
         VALUES (?, ?, ?, ?, ?, ?)`,
      )
      .run(id, input.sessionId, input.turnId ?? null, input.kind, input.data, nowMillis(this.#now));
    const row = this.db
      .query<PlatformTraceRow, [string]>(`SELECT ${TRACE_COLUMNS} FROM platform_traces WHERE id = ?`)
      .get(id);
    if (!row) throw new Error(`Created trace ${id} could not be read back`);
    return row;
  }
}
