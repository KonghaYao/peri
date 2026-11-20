import type { Database } from 'bun:sqlite';
import type { SessionItemWire } from '../sessions/serialize';

/**
 * Read-only seam for child-thread history kept by the session Store (`threads` / `messages`).
 *
 * The platform routes only receive a database handle, so the Store client is installed by the
 * process that owns it. Without an installed reader the local platform tables are the source;
 * with one installed, the Store is authoritative and every failure stays visible: a schema probe
 * mismatch answers `store_schema_unsupported` (409) and an unreachable Store degrades the history
 * endpoints to an empty page carrying `x-platform-degraded: subagent_history_unavailable`.
 *
 * Readers must probe the columns they read before answering; the probe is a `table_info` query in
 * the SQLite-backed deployment. Nothing here synthesizes history when the reader cannot answer.
 */
export interface StoreHistoryTurn {
  readonly id: string;
  readonly status: string;
  /** Unix seconds, matching the platform wire format. */
  readonly created_at: number;
  readonly started_at: number | null;
  readonly completed_at: number | null;
  readonly items: readonly SessionItemWire[];
}

export interface StoreThreadHistory {
  readonly turns: readonly StoreHistoryTurn[];
}

export interface SubagentHistoryReader {
  /** Rejects with `StoreSchemaUnsupportedError` when the Store columns differ from the contract. */
  probe(): Promise<void> | void;
  /**
   * Reads one child thread. `threadId` is the subagent instance ID, which is also the child
   * thread ID (`threads.id` matches the projection's `instance_id`).
   */
  read(threadId: string): Promise<StoreThreadHistory>;
}

/** The Store answered but its schema cannot serve the documented subagent history. */
export class StoreSchemaUnsupportedError extends Error {
  readonly code = 'store_schema_unsupported';

  constructor(message: string) {
    super(message);
    this.name = 'StoreSchemaUnsupportedError';
  }
}

/** The Store is configured but could not answer; callers must degrade instead of inventing rows. */
export class StoreHistoryUnavailableError extends Error {
  readonly code = 'subagent_history_unavailable';

  constructor(message: string) {
    super(message);
    this.name = 'StoreHistoryUnavailableError';
  }
}

const READERS = new WeakMap<Database, SubagentHistoryReader>();

/** Installs (or replaces) the Store reader for one database; returns a restore handle for tests. */
export function installSubagentHistoryReader(db: Database, reader: SubagentHistoryReader): () => void {
  const previous = READERS.get(db);
  READERS.set(db, reader);
  return () => {
    if (previous === undefined) READERS.delete(db);
    else READERS.set(db, previous);
  };
}

export function subagentHistoryReaderFor(db: Database): SubagentHistoryReader | null {
  return READERS.get(db) ?? null;
}
