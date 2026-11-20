import type { Database } from 'bun:sqlite';
import { newPlatformId } from '../ids';
import { EMPTY_JSON_ARRAY, EMPTY_JSON_OBJECT } from '../json';
import type { PlatformEnvironmentType } from './environments';
import type { ListQuery } from './shared';
import { nowMillis, removeById } from './shared';

export type PlatformSessionStatus = 'idle' | 'in_progress' | 'requires_action' | 'failed';
export type PlatformTurnStatus = 'queued' | 'in_progress' | 'waiting' | 'completed' | 'failed' | 'cancelled';

/**
 * Platform tables persist `created_at` / `last_active_at` as Unix milliseconds. The API layer
 * converts them to Unix seconds when serializing responses.
 */
export interface PlatformSessionRow {
  readonly id: string;
  readonly agent_id: string;
  readonly agent_config: string;
  readonly environment_type: PlatformEnvironmentType;
  readonly environment_id: string | null;
  readonly metadata: string;
  readonly vault_ids: string;
  readonly status: PlatformSessionStatus;
  readonly error: string | null;
  readonly required_actions: string;
  readonly usage: string | null;
  readonly prompt_cache_options: string | null;
  readonly review: string | null;
  readonly environment_reset_count: number;
  readonly created_at: number;
  readonly last_active_at: number;
}

export interface NewPlatformSession {
  readonly id?: string;
  readonly agentId: string;
  readonly agentConfig: string;
  readonly environmentType: PlatformEnvironmentType;
  readonly environmentId?: string | null;
  readonly metadata?: string;
  readonly vaultIds?: string;
  readonly status?: PlatformSessionStatus;
  readonly requiredActions?: string;
  readonly usage?: string | null;
  readonly promptCacheOptions?: string | null;
  readonly review?: string | null;
}

export interface PlatformSessionUpdate {
  readonly agentConfig: string | null;
  readonly environmentType: PlatformEnvironmentType | null;
  readonly environmentId: string | null;
  readonly metadata: string | null;
  readonly vaultIds: string | null;
  readonly requiredActions: string | null;
  readonly usage: string | null;
  readonly promptCacheOptions: string | null;
  readonly review: string | null;
  readonly environmentResetCount: number | null;
}

export interface PlatformSessionRepositoryOptions {
  readonly now?: () => number;
}

/** Exact-match filters documented for session listing; omitted fields do not filter. */
export interface PlatformSessionFilter {
  readonly agentId?: string | null;
  readonly metadata?: Readonly<Record<string, string>>;
}

/** Quotes a metadata key as a JSON path; callers validate the key before it reaches here. */
function metadataPath(key: string): string {
  return `$."${key.replace(/"/g, '')}"`;
}

const SESSION_COLUMNS =
  'id, agent_id, agent_config, environment_type, environment_id, metadata, vault_ids, status, error, required_actions, usage, prompt_cache_options, review, environment_reset_count, created_at, last_active_at';

export class PlatformSessionsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformSessionRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  /**
   * Keyset page over sessions with optional exact-match filters.
   * With no filters this is exactly `buildListStatement` over `platform_sessions`.
   */
  list(query: ListQuery, filter: PlatformSessionFilter = {}): PlatformSessionRow[] {
    const clauses: string[] = [];
    const params: (string | number)[] = [];
    if (filter.agentId !== undefined && filter.agentId !== null) {
      clauses.push('agent_id = ?');
      params.push(filter.agentId);
    }
    for (const [key, value] of Object.entries(filter.metadata ?? {})) {
      clauses.push('json_extract(metadata, ?) = ?');
      params.push(metadataPath(key), value);
    }
    if (query.after !== null) {
      clauses.push(`id ${query.order === 'asc' ? '>' : '<'} ?`);
      params.push(query.after);
    }
    const direction = query.order === 'asc' ? 'ASC' : 'DESC';
    const where = clauses.length === 0 ? '' : `WHERE ${clauses.join(' AND ')}`;
    params.push(query.limit);
    return this.db
      .query<PlatformSessionRow, (string | number)[]>(
        `SELECT ${SESSION_COLUMNS} FROM platform_sessions ${where} ORDER BY id ${direction} LIMIT ?`,
      )
      .all(...params);
  }

  get(id: string): PlatformSessionRow | null {
    const row = this.db
      .query<PlatformSessionRow, [string]>(`SELECT ${SESSION_COLUMNS} FROM platform_sessions WHERE id = ?`)
      .get(id);
    return row ?? null;
  }

  create(input: NewPlatformSession): PlatformSessionRow {
    const id = input.id ?? newPlatformId();
    const now = nowMillis(this.#now);
    this.db
      .query(
        `INSERT INTO platform_sessions (
          id, agent_id, agent_config, environment_type, environment_id, metadata, vault_ids,
          status, error, required_actions, usage, prompt_cache_options, review,
          environment_reset_count, created_at, last_active_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, 0, ?, ?)`,
      )
      .run(
        id,
        input.agentId,
        input.agentConfig,
        input.environmentType,
        input.environmentId ?? null,
        input.metadata ?? EMPTY_JSON_OBJECT,
        input.vaultIds ?? EMPTY_JSON_ARRAY,
        input.status ?? 'idle',
        input.requiredActions ?? EMPTY_JSON_ARRAY,
        input.usage ?? null,
        input.promptCacheOptions ?? null,
        input.review ?? null,
        now,
        now,
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created session ${id} could not be read back`);
    return row;
  }

  update(id: string, input: PlatformSessionUpdate): PlatformSessionRow | null {
    this.db
      .query(
        `UPDATE platform_sessions SET
          agent_config = COALESCE(?, agent_config),
          environment_type = COALESCE(?, environment_type),
          environment_id = COALESCE(?, environment_id),
          metadata = COALESCE(?, metadata),
          vault_ids = COALESCE(?, vault_ids),
          required_actions = COALESCE(?, required_actions),
          usage = COALESCE(?, usage),
          prompt_cache_options = COALESCE(?, prompt_cache_options),
          review = COALESCE(?, review),
          environment_reset_count = COALESCE(?, environment_reset_count),
          last_active_at = ?
        WHERE id = ?`,
      )
      .run(
        input.agentConfig,
        input.environmentType,
        input.environmentId,
        input.metadata,
        input.vaultIds,
        input.requiredActions,
        input.usage,
        input.promptCacheOptions,
        input.review,
        input.environmentResetCount,
        nowMillis(this.#now),
        id,
      );
    return this.get(id);
  }

  setStatus(
    id: string,
    status: PlatformSessionStatus,
    error: string | null = null,
  ): PlatformSessionRow | null {
    this.db
      .query('UPDATE platform_sessions SET status = ?, error = ?, last_active_at = ? WHERE id = ?')
      .run(status, error, nowMillis(this.#now), id);
    return this.get(id);
  }

  delete(id: string): boolean {
    return removeById(this.db, 'platform_sessions', id);
  }
}

export interface PlatformTurnRow {
  readonly id: string;
  readonly session_id: string;
  readonly agent_id: string;
  readonly subagent_id: string | null;
  readonly runtime_turn_id: string;
  readonly binding_revision: number;
  readonly status: PlatformTurnStatus;
  readonly created_at: number;
  readonly started_at: number | null;
  readonly completed_at: number | null;
  readonly error_code: string | null;
  readonly error_message: string | null;
  readonly stop_reason: string | null;
  readonly usage: string | null;
  readonly prompt_cache_options: string | null;
}

export interface NewPlatformTurn {
  readonly id?: string;
  readonly sessionId: string;
  readonly agentId: string;
  readonly subagentId?: string | null;
  readonly runtimeTurnId: string;
  readonly bindingRevision: number;
  readonly status?: PlatformTurnStatus;
  readonly startedAt?: number | null;
}

export interface CompletePlatformTurn {
  readonly completedAt?: number;
  readonly stopReason?: string | null;
  readonly errorCode?: string | null;
  readonly errorMessage?: string | null;
  readonly usage?: string | null;
  readonly promptCacheOptions?: string | null;
}

const TURN_COLUMNS =
  'id, session_id, agent_id, subagent_id, runtime_turn_id, binding_revision, status, created_at, started_at, completed_at, error_code, error_message, stop_reason, usage, prompt_cache_options';

export class PlatformTurnsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformSessionRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  get(id: string): PlatformTurnRow | null {
    const row = this.db
      .query<PlatformTurnRow, [string]>(`SELECT ${TURN_COLUMNS} FROM platform_turns WHERE id = ?`)
      .get(id);
    return row ?? null;
  }

  listBySession(sessionId: string): PlatformTurnRow[] {
    return this.db
      .query<PlatformTurnRow, [string]>(
        `SELECT ${TURN_COLUMNS} FROM platform_turns
         WHERE session_id = ? ORDER BY created_at ASC, id ASC`,
      )
      .all(sessionId);
  }

  /** Locates the recorded turn for one runtime turn identity (the uniqueness key of the table). */
  findByRuntime(sessionId: string, bindingRevision: number, runtimeTurnId: string): PlatformTurnRow | null {
    const row = this.db
      .query<PlatformTurnRow, [string, number, string]>(
        `SELECT ${TURN_COLUMNS} FROM platform_turns
         WHERE session_id = ? AND binding_revision = ? AND runtime_turn_id = ?`,
      )
      .get(sessionId, bindingRevision, runtimeTurnId);
    return row ?? null;
  }

  /** Finds the recorded row for a runtime turn id, newest generation first. */
  findLatestByRuntime(sessionId: string, runtimeTurnId: string): PlatformTurnRow | null {
    const row = this.db
      .query<PlatformTurnRow, [string, string]>(
        `SELECT ${TURN_COLUMNS} FROM platform_turns
         WHERE session_id = ? AND runtime_turn_id = ?
         ORDER BY created_at DESC, id DESC LIMIT 1`,
      )
      .get(sessionId, runtimeTurnId);
    return row ?? null;
  }

  /** Turns that never reached a terminal state; used by the startup convergence sweep. */
  listNonTerminal(): PlatformTurnRow[] {
    return this.db
      .query<PlatformTurnRow, []>(
        `SELECT ${TURN_COLUMNS} FROM platform_turns
         WHERE status NOT IN ('completed', 'failed', 'cancelled') ORDER BY created_at ASC, id ASC`,
      )
      .all();
  }

  listBySubagent(sessionId: string, subagentId: string): PlatformTurnRow[] {
    return this.db
      .query<PlatformTurnRow, [string, string]>(
        `SELECT ${TURN_COLUMNS} FROM platform_turns
         WHERE session_id = ? AND subagent_id = ? ORDER BY created_at ASC, id ASC`,
      )
      .all(sessionId, subagentId);
  }

  create(input: NewPlatformTurn): PlatformTurnRow {
    const id = input.id ?? newPlatformId();
    this.db
      .query(
        `INSERT INTO platform_turns (
          id, session_id, agent_id, subagent_id, runtime_turn_id, binding_revision,
          status, created_at, started_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.sessionId,
        input.agentId,
        input.subagentId ?? null,
        input.runtimeTurnId,
        input.bindingRevision,
        input.status ?? 'queued',
        nowMillis(this.#now),
        input.startedAt ?? null,
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created turn ${id} could not be read back`);
    return row;
  }

  setStatus(id: string, status: PlatformTurnStatus, startedAt: number | null = null): PlatformTurnRow | null {
    this.db
      .query('UPDATE platform_turns SET status = ?, started_at = COALESCE(?, started_at) WHERE id = ?')
      .run(status, startedAt, id);
    return this.get(id);
  }

  complete(
    id: string,
    status: Extract<PlatformTurnStatus, 'completed' | 'failed' | 'cancelled'>,
    options: CompletePlatformTurn = {},
  ): PlatformTurnRow | null {
    this.db
      .query(
        `UPDATE platform_turns SET
          status = ?,
          completed_at = ?,
          stop_reason = ?,
          error_code = ?,
          error_message = ?,
          usage = ?,
          prompt_cache_options = ?
        WHERE id = ?`,
      )
      .run(
        status,
        options.completedAt ?? nowMillis(this.#now),
        options.stopReason ?? null,
        options.errorCode ?? null,
        options.errorMessage ?? null,
        options.usage ?? null,
        options.promptCacheOptions ?? null,
        id,
      );
    return this.get(id);
  }
}

export interface PlatformItemRow {
  readonly id: string;
  readonly session_id: string;
  readonly turn_id: string;
  readonly output_index: number;
  readonly type: string;
  readonly status: string;
  readonly call_id: string | null;
  readonly payload: string;
  readonly created_at: number;
  readonly completed_at: number | null;
}

export interface NewPlatformItem {
  readonly id?: string;
  readonly sessionId: string;
  readonly turnId: string;
  readonly outputIndex: number;
  readonly type: string;
  readonly status: string;
  readonly callId?: string | null;
  readonly payload: string;
  readonly completedAt?: number | null;
}

export interface PlatformItemUpdate {
  readonly status?: string;
  readonly payload?: string;
  readonly callId?: string | null;
  readonly completedAt?: number | null;
}

const ITEM_COLUMNS =
  'i.id, i.session_id, i.turn_id, i.output_index, i.type, i.status, i.call_id, i.payload, i.created_at, i.completed_at';

export class PlatformItemsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformSessionRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  get(id: string): PlatformItemRow | null {
    const row = this.db
      .query<PlatformItemRow, [string]>(`SELECT ${ITEM_COLUMNS} FROM platform_items i WHERE i.id = ?`)
      .get(id);
    return row ?? null;
  }

  listBySession(sessionId: string): PlatformItemRow[] {
    return this.db
      .query<PlatformItemRow, [string]>(
        `SELECT ${ITEM_COLUMNS} FROM platform_items i
         JOIN platform_turns t ON t.id = i.turn_id
         WHERE i.session_id = ?
         ORDER BY t.created_at ASC, t.id ASC, i.output_index ASC`,
      )
      .all(sessionId);
  }

  listByTurn(turnId: string): PlatformItemRow[] {
    return this.db
      .query<PlatformItemRow, [string]>(
        `SELECT ${ITEM_COLUMNS} FROM platform_items i
         WHERE i.turn_id = ? ORDER BY i.output_index ASC`,
      )
      .all(turnId);
  }

  listBySubagent(sessionId: string, subagentId: string): PlatformItemRow[] {
    return this.db
      .query<PlatformItemRow, [string, string]>(
        `SELECT ${ITEM_COLUMNS} FROM platform_items i
         JOIN platform_turns t ON t.id = i.turn_id
         WHERE i.session_id = ? AND t.subagent_id = ?
         ORDER BY t.created_at ASC, t.id ASC, i.output_index ASC`,
      )
      .all(sessionId, subagentId);
  }

  create(input: NewPlatformItem): PlatformItemRow {
    const id = input.id ?? newPlatformId();
    this.db
      .query(
        `INSERT INTO platform_items (
          id, session_id, turn_id, output_index, type, status, call_id, payload, created_at, completed_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.sessionId,
        input.turnId,
        input.outputIndex,
        input.type,
        input.status,
        input.callId ?? null,
        input.payload,
        nowMillis(this.#now),
        input.completedAt ?? null,
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created item ${id} could not be read back`);
    return row;
  }

  /** Item state is append-only in the wire format but mutable in storage until it completes. */
  update(id: string, input: PlatformItemUpdate): PlatformItemRow | null {
    this.db
      .query(
        `UPDATE platform_items SET
          status = COALESCE(?, status),
          payload = COALESCE(?, payload),
          call_id = COALESCE(?, call_id),
          completed_at = COALESCE(?, completed_at)
        WHERE id = ?`,
      )
      .run(input.status ?? null, input.payload ?? null, input.callId ?? null, input.completedAt ?? null, id);
    return this.get(id);
  }

  /** Items still marked in progress; the startup sweep closes them as `incomplete`. */
  listNonTerminal(): PlatformItemRow[] {
    return this.db
      .query<PlatformItemRow, []>(
        `SELECT ${ITEM_COLUMNS} FROM platform_items i
         WHERE i.status NOT IN ('completed', 'incomplete') ORDER BY i.created_at ASC, i.id ASC`,
      )
      .all();
  }
}

export interface PlatformEventRow {
  readonly seq: number;
  readonly event_id: string;
  readonly session_id: string;
  readonly type: string;
  readonly turn_id: string | null;
  readonly item_id: string | null;
  readonly data: string;
  readonly created_at: number;
}

export interface NewPlatformEvent {
  readonly eventId?: string;
  readonly sessionId: string;
  readonly type: string;
  readonly turnId?: string | null;
  readonly itemId?: string | null;
  readonly data: string;
}

const EVENT_COLUMNS = 'seq, event_id, session_id, type, turn_id, item_id, data, created_at';

export class PlatformEventsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformSessionRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  append(input: NewPlatformEvent): PlatformEventRow {
    const result = this.db
      .query(
        `INSERT INTO platform_events (event_id, session_id, type, turn_id, item_id, data, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        input.eventId ?? newPlatformId(),
        input.sessionId,
        input.type,
        input.turnId ?? null,
        input.itemId ?? null,
        input.data,
        nowMillis(this.#now),
      );
    const row = this.db
      .query<PlatformEventRow, [number]>(`SELECT ${EVENT_COLUMNS} FROM platform_events WHERE seq = ?`)
      .get(Number(result.lastInsertRowid));
    if (!row) throw new Error('Created event could not be read back');
    return row;
  }

  /** Replays a session's event log in order; `afterSeq` makes replays resumable and idempotent. */
  listBySession(sessionId: string, afterSeq = 0): PlatformEventRow[] {
    return this.db
      .query<PlatformEventRow, [string, number]>(
        `SELECT ${EVENT_COLUMNS} FROM platform_events
         WHERE session_id = ? AND seq > ? ORDER BY seq ASC`,
      )
      .all(sessionId, afterSeq);
  }

  /** Resolves an SSE `Last-Event-ID` that carries the public event id instead of the log position. */
  findByEventId(sessionId: string, eventId: string): PlatformEventRow | null {
    const row = this.db
      .query<PlatformEventRow, [string, string]>(
        `SELECT ${EVENT_COLUMNS} FROM platform_events WHERE session_id = ? AND event_id = ?`,
      )
      .get(sessionId, eventId);
    return row ?? null;
  }
}

export interface PlatformInputReceiptRow {
  readonly session_id: string;
  readonly idempotency_key: string;
  readonly digest: string;
  readonly request_id: string;
  readonly created_at: number;
}

export interface NewPlatformInputReceipt {
  readonly sessionId: string;
  readonly idempotencyKey: string;
  readonly digest: string;
  readonly requestId: string;
}

const RECEIPT_COLUMNS = 'session_id, idempotency_key, digest, request_id, created_at';

/** Durable `Idempotency-Key` outcomes for session input submission. */
export class PlatformInputReceiptsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformSessionRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  get(sessionId: string, idempotencyKey: string): PlatformInputReceiptRow | null {
    const row = this.db
      .query<PlatformInputReceiptRow, [string, string]>(
        `SELECT ${RECEIPT_COLUMNS} FROM platform_input_receipts
         WHERE session_id = ? AND idempotency_key = ?`,
      )
      .get(sessionId, idempotencyKey);
    return row ?? null;
  }

  insert(input: NewPlatformInputReceipt): PlatformInputReceiptRow {
    this.db
      .query(
        `INSERT INTO platform_input_receipts (session_id, idempotency_key, digest, request_id, created_at)
         VALUES (?, ?, ?, ?, ?)`,
      )
      .run(input.sessionId, input.idempotencyKey, input.digest, input.requestId, nowMillis(this.#now));
    const row = this.get(input.sessionId, input.idempotencyKey);
    if (!row) throw new Error('Created input receipt could not be read back');
    return row;
  }

  /** A failed submission must not poison the key: the caller retries the same body. */
  delete(sessionId: string, idempotencyKey: string): void {
    this.db
      .query('DELETE FROM platform_input_receipts WHERE session_id = ? AND idempotency_key = ?')
      .run(sessionId, idempotencyKey);
  }
}
