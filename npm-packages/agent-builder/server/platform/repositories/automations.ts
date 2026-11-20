import type { Database } from 'bun:sqlite';
import { newPlatformId } from '../ids';
import { EMPTY_JSON_OBJECT } from '../json';
import type { ListQuery } from './shared';
import { buildListStatement, nowMillis, removeById } from './shared';

export type PlatformAutomationType = 'periodic' | 'once';
export type PlatformAutomationStatus = 'active' | 'paused' | 'completed';
export type PlatformAutomationInvocationSource = 'schedule' | 'manual';
export type PlatformAutomationInvocationStatus = 'queued' | 'completed' | 'failed' | 'skipped';

export interface PlatformAutomationRow {
  readonly id: string;
  readonly type: PlatformAutomationType;
  readonly name: string;
  readonly target: string;
  readonly metadata: string;
  readonly expression: string | null;
  readonly timezone: string | null;
  readonly run_at: number | null;
  readonly status: PlatformAutomationStatus;
  readonly pause_reason: string | null;
  readonly next_run_at: number | null;
  readonly created_by: string;
  readonly created_at: number;
  readonly updated_at: number;
}

export interface NewPlatformAutomation {
  readonly id?: string;
  readonly type: PlatformAutomationType;
  readonly name: string;
  readonly target: string;
  readonly metadata?: string;
  readonly expression?: string | null;
  readonly timezone?: string | null;
  readonly runAt?: number | null;
  readonly status?: PlatformAutomationStatus;
  readonly nextRunAt?: number | null;
  readonly createdBy: string;
}

export interface PlatformAutomationUpdate {
  readonly type: PlatformAutomationType | null;
  readonly name: string | null;
  readonly target: string | null;
  readonly metadata: string | null;
  readonly expression: string | null;
  readonly timezone: string | null;
  readonly runAt: number | null;
  readonly status: PlatformAutomationStatus | null;
  readonly pauseReason: string | null;
  readonly nextRunAt: number | null;
}

const AUTOMATION_COLUMNS =
  'id, type, name, target, metadata, expression, timezone, run_at, status, pause_reason, next_run_at, created_by, created_at, updated_at';

export interface PlatformAutomationsRepositoryOptions {
  readonly now?: () => number;
}

export class PlatformAutomationsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformAutomationsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  list(query: ListQuery): PlatformAutomationRow[] {
    const { sql, params } = buildListStatement('platform_automations', AUTOMATION_COLUMNS, query);
    return this.db.query<PlatformAutomationRow, (string | number)[]>(sql).all(...params);
  }

  /** Full inventory used by the filtered list; the table is small and ordering stays in one place. */
  listAll(): PlatformAutomationRow[] {
    return this.db
      .query<PlatformAutomationRow, []>(
        `SELECT ${AUTOMATION_COLUMNS} FROM platform_automations ORDER BY id ASC`,
      )
      .all();
  }

  /** Due schedules for one scheduler tick; only active rows with a reached `next_run_at`. */
  listDue(now: number): PlatformAutomationRow[] {
    return this.db
      .query<PlatformAutomationRow, [number]>(
        `SELECT ${AUTOMATION_COLUMNS} FROM platform_automations
         WHERE status = 'active' AND next_run_at IS NOT NULL AND next_run_at <= ?
         ORDER BY next_run_at ASC, id ASC`,
      )
      .all(now);
  }

  get(id: string): PlatformAutomationRow | null {
    const row = this.db
      .query<PlatformAutomationRow, [string]>(
        `SELECT ${AUTOMATION_COLUMNS} FROM platform_automations WHERE id = ?`,
      )
      .get(id);
    return row ?? null;
  }

  create(input: NewPlatformAutomation): PlatformAutomationRow {
    const id = input.id ?? newPlatformId();
    const now = nowMillis(this.#now);
    this.db
      .query(
        `INSERT INTO platform_automations (
          id, type, name, target, metadata, expression, timezone, run_at, status, next_run_at,
          created_by, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.type,
        input.name,
        input.target,
        input.metadata ?? EMPTY_JSON_OBJECT,
        input.expression ?? null,
        input.timezone ?? null,
        input.runAt ?? null,
        input.status ?? 'active',
        input.nextRunAt ?? null,
        input.createdBy,
        now,
        now,
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created automation ${id} could not be read back`);
    return row;
  }

  update(id: string, input: PlatformAutomationUpdate): PlatformAutomationRow | null {
    this.db
      .query(
        `UPDATE platform_automations SET
          type = COALESCE(?, type),
          name = COALESCE(?, name),
          target = COALESCE(?, target),
          metadata = COALESCE(?, metadata),
          expression = COALESCE(?, expression),
          timezone = COALESCE(?, timezone),
          run_at = COALESCE(?, run_at),
          status = COALESCE(?, status),
          pause_reason = COALESCE(?, pause_reason),
          next_run_at = COALESCE(?, next_run_at),
          updated_at = ?
        WHERE id = ?`,
      )
      .run(
        input.type,
        input.name,
        input.target,
        input.metadata,
        input.expression,
        input.timezone,
        input.runAt,
        input.status,
        input.pauseReason,
        input.nextRunAt,
        nowMillis(this.#now),
        id,
      );
    return this.get(id);
  }

  pause(id: string, reason: string | null): PlatformAutomationRow | null {
    this.db
      .query(
        `UPDATE platform_automations SET status = 'paused', pause_reason = ?, updated_at = ?
         WHERE id = ?`,
      )
      .run(reason, nowMillis(this.#now), id);
    return this.get(id);
  }

  /** Advances the schedule without touching status; `null` clears a completed one-time schedule. */
  setNextRunAt(id: string, nextRunAt: number | null): PlatformAutomationRow | null {
    this.db
      .query('UPDATE platform_automations SET next_run_at = ?, updated_at = ? WHERE id = ?')
      .run(nextRunAt, nowMillis(this.#now), id);
    return this.get(id);
  }

  markCompleted(id: string): PlatformAutomationRow | null {
    this.db
      .query(
        `UPDATE platform_automations SET status = 'completed', next_run_at = NULL, updated_at = ?
         WHERE id = ?`,
      )
      .run(nowMillis(this.#now), id);
    return this.get(id);
  }

  resume(id: string): PlatformAutomationRow | null {
    this.db
      .query(
        `UPDATE platform_automations SET status = 'active', pause_reason = NULL, updated_at = ?
         WHERE id = ?`,
      )
      .run(nowMillis(this.#now), id);
    return this.get(id);
  }

  delete(id: string): boolean {
    return removeById(this.db, 'platform_automations', id);
  }
}

export interface PlatformAutomationInvocationRow {
  readonly id: string;
  readonly automation_id: string;
  readonly source: PlatformAutomationInvocationSource;
  readonly scheduled_at: number | null;
  readonly session_id: string | null;
  readonly status: PlatformAutomationInvocationStatus;
  readonly skip_reason: string | null;
  readonly error_code: string | null;
  readonly error_message: string | null;
  readonly created_at: number;
  readonly completed_at: number | null;
}

export interface NewPlatformAutomationInvocation {
  readonly id?: string;
  readonly automationId: string;
  readonly source: PlatformAutomationInvocationSource;
  readonly scheduledAt?: number | null;
  readonly sessionId?: string | null;
  readonly status?: PlatformAutomationInvocationStatus;
}

export interface CompletePlatformAutomationInvocation {
  readonly status: Extract<PlatformAutomationInvocationStatus, 'completed' | 'failed' | 'skipped'>;
  readonly completedAt?: number;
  readonly sessionId?: string | null;
  readonly skipReason?: string | null;
  readonly errorCode?: string | null;
  readonly errorMessage?: string | null;
}

const INVOCATION_COLUMNS =
  'id, automation_id, source, scheduled_at, session_id, status, skip_reason, error_code, error_message, created_at, completed_at';

export class PlatformAutomationInvocationsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformAutomationsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  get(id: string): PlatformAutomationInvocationRow | null {
    const row = this.db
      .query<PlatformAutomationInvocationRow, [string]>(
        `SELECT ${INVOCATION_COLUMNS} FROM platform_automation_invocations WHERE id = ?`,
      )
      .get(id);
    return row ?? null;
  }

  /** Looks up the scheduled invocation; the unique `(automation_id, scheduled_at)` pair dedupes ticks. */
  getBySchedule(automationId: string, scheduledAt: number): PlatformAutomationInvocationRow | null {
    const row = this.db
      .query<PlatformAutomationInvocationRow, [string, number]>(
        `SELECT ${INVOCATION_COLUMNS} FROM platform_automation_invocations
         WHERE automation_id = ? AND scheduled_at = ?`,
      )
      .get(automationId, scheduledAt);
    return row ?? null;
  }

  listByAutomation(automationId: string): PlatformAutomationInvocationRow[] {
    return this.db
      .query<PlatformAutomationInvocationRow, [string]>(
        `SELECT ${INVOCATION_COLUMNS} FROM platform_automation_invocations
         WHERE automation_id = ? ORDER BY created_at ASC, id ASC`,
      )
      .all(automationId);
  }

  /** True once a schedule occurrence exists; one-time `run_at` is locked from that point. */
  hasScheduled(automationId: string): boolean {
    const row = this.db
      .query<{ id: string }, [string]>(
        `SELECT id FROM platform_automation_invocations
         WHERE automation_id = ? AND source = 'schedule' LIMIT 1`,
      )
      .get(automationId);
    return row !== undefined;
  }

  create(input: NewPlatformAutomationInvocation): PlatformAutomationInvocationRow {
    const id = input.id ?? newPlatformId();
    this.db
      .query(
        `INSERT INTO platform_automation_invocations (
          id, automation_id, source, scheduled_at, session_id, status, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.automationId,
        input.source,
        input.scheduledAt ?? null,
        input.sessionId ?? null,
        input.status ?? 'queued',
        nowMillis(this.#now),
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created invocation ${id} could not be read back`);
    return row;
  }

  complete(id: string, input: CompletePlatformAutomationInvocation): PlatformAutomationInvocationRow | null {
    this.db
      .query(
        `UPDATE platform_automation_invocations SET
          status = ?,
          completed_at = ?,
          session_id = COALESCE(?, session_id),
          skip_reason = ?,
          error_code = ?,
          error_message = ?
        WHERE id = ?`,
      )
      .run(
        input.status,
        input.completedAt ?? nowMillis(this.#now),
        input.sessionId ?? null,
        input.skipReason ?? null,
        input.errorCode ?? null,
        input.errorMessage ?? null,
        id,
      );
    return this.get(id);
  }
}
