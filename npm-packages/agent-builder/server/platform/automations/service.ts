import { serializeMetadataColumn } from '../json';
import { type PageRequest, type PlatformListPage, paginate } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import type {
  CompletePlatformAutomationInvocation,
  PlatformAutomationInvocationRow,
  PlatformAutomationRow,
} from '../repositories/automations';
import {
  CronParseError,
  type CronSchedule,
  CronTimeZoneError,
  isSupportedTimeZone,
  nextCronOccurrence,
  parseCron,
} from './cron';
import { AutomationHttpError } from './errors';
import {
  type AutomationTargetParam,
  type CreateAutomationParams,
  parseStoredTarget,
  type StoredAutomationTarget,
  serializeTarget,
  targetEnvironmentDegradation,
  type UpdateAutomationParams,
} from './schemas';
import {
  type AutomationInvocationResource,
  type AutomationResource,
  serializeAutomation,
  serializeInvocation,
} from './serialize';
import type { AutomationSessionPort } from './session-port';

const AUTOMATION_NOT_FOUND = 'Automation not found';
const INVOCATION_NOT_FOUND = 'Invocation not found';
const CREATED_BY_DEVELOPER = JSON.stringify({ type: 'developer' });

/**
 * A periodic occurrence more than one minute late is treated as a missed tick: the schedule moves
 * past it instead of backfilling every skipped occurrence. A one-time schedule still runs because
 * its single occurrence has no replacement.
 */
const DEFAULT_MISSED_GRACE_MS = 60_000;

export interface AutomationServiceOptions {
  readonly repos: PlatformRepositories;
  /** Null until the process installs the session runtime; deliveries then fail visibly. */
  readonly port: AutomationSessionPort | null;
  readonly now?: () => number;
  readonly missedGraceMs?: number;
}

function invalid(message: string, param: string | null = null): AutomationHttpError {
  return new AutomationHttpError('invalid_input', message, 400, param);
}

function compareIds(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

/** Automation CRUD, delivery and the in-process scheduler tick. */
export class AutomationService {
  readonly #repos: PlatformRepositories;
  readonly #port: AutomationSessionPort | null;
  readonly #now: () => number;
  readonly #missedGraceMs: number;
  readonly #running = new Set<string>();
  readonly #pending = new Set<Promise<void>>();

  constructor(options: AutomationServiceOptions) {
    this.#repos = options.repos;
    this.#port = options.port;
    this.#now = options.now ?? Date.now;
    this.#missedGraceMs = options.missedGraceMs ?? DEFAULT_MISSED_GRACE_MS;
  }

  list(page: PageRequest, sessionId: string | null): PlatformListPage<AutomationResource> {
    const rows = this.#repos.automations.listAll();
    const filtered =
      sessionId === null
        ? rows
        : rows.filter((row) => {
            try {
              const target = parseStoredTarget(row.target);
              return target.type === 'existing_session' && target.session_id === sessionId;
            } catch {
              return false;
            }
          });
    const direction = page.order === 'asc' ? 1 : -1;
    filtered.sort((left, right) => direction * compareIds(left.id, right.id));
    const stored = paginate(filtered, page, (row) => row.id);
    return { ...stored, data: stored.data.map(serializeAutomation) };
  }

  get(id: string): AutomationResource {
    return serializeAutomation(this.#require(id));
  }

  /** Target degradations; routes report them as `x-platform-degraded` instead of a second shape. */
  degradation(id: string): readonly string[] {
    const target = parseStoredTarget(this.#require(id).target);
    if (target.type !== 'new_session') return [];
    return targetEnvironmentDegradation(target.environment);
  }

  create(params: CreateAutomationParams): AutomationResource {
    this.#validateTarget(params.target);
    const now = this.#now();
    const shared = {
      name: params.name,
      target: serializeTarget(params.target),
      createdBy: CREATED_BY_DEVELOPER,
      ...(params.metadata === undefined ? {} : { metadata: serializeMetadataColumn(params.metadata) }),
    };

    if (params.type === 'periodic') {
      const nextRunAt = this.#nextAfter(params.expression, params.timezone, now);
      return serializeAutomation(
        this.#repos.automations.create({
          ...shared,
          type: 'periodic',
          expression: params.expression,
          timezone: params.timezone,
          nextRunAt,
        }),
      );
    }

    const runAt = params.run_at * 1000;
    if (runAt <= now) throw invalid('run_at must be in the future', 'run_at');
    return serializeAutomation(
      this.#repos.automations.create({
        ...shared,
        type: 'once',
        runAt,
        nextRunAt: runAt,
      }),
    );
  }

  update(id: string, params: UpdateAutomationParams): AutomationResource {
    const row = this.#require(id);
    const now = this.#now();
    const target = params.target === undefined ? null : serializeTarget(params.target);
    if (params.target !== undefined) this.#validateTarget(params.target);
    const metadata = params.metadata === undefined ? null : serializeMetadataColumn(params.metadata);

    let expression: string | null = null;
    let timezone: string | null = null;
    let runAt: number | null = null;
    let nextRunAt: number | null = null;

    if (row.type === 'periodic') {
      if (params.run_at !== undefined) {
        throw invalid('run_at is only accepted for one-time automations', 'run_at');
      }
      if (params.expression !== undefined || params.timezone !== undefined) {
        const nextExpression = params.expression ?? row.expression ?? '';
        const nextTimezone = params.timezone ?? row.timezone ?? '';
        const schedule = this.#parseSchedule(nextExpression, nextTimezone);
        if (row.status === 'active') {
          nextRunAt = this.#nextOccurrence(schedule, nextTimezone, now);
        }
      }
      expression = params.expression ?? null;
      timezone = params.timezone ?? null;
    } else {
      if (params.expression !== undefined) {
        throw invalid('expression is only accepted for periodic automations', 'expression');
      }
      if (params.timezone !== undefined) {
        throw invalid('timezone is only accepted for periodic automations', 'timezone');
      }
      if (params.run_at !== undefined) {
        if (this.#repos.automationInvocations.hasScheduled(id)) {
          throw new AutomationHttpError(
            'run_at_locked',
            'run_at cannot change after a scheduled invocation exists',
            409,
            'run_at',
          );
        }
        const candidate = params.run_at * 1000;
        if (candidate <= now) throw invalid('run_at must be in the future', 'run_at');
        runAt = candidate;
        nextRunAt = candidate;
      }
    }

    const updated = this.#repos.automations.update(id, {
      type: null,
      name: params.name ?? null,
      target,
      metadata,
      expression,
      timezone,
      runAt,
      status: null,
      pauseReason: null,
      nextRunAt,
    });
    if (updated === null) throw new AutomationHttpError('not_found', AUTOMATION_NOT_FOUND, 404);
    return serializeAutomation(updated);
  }

  pause(id: string): AutomationResource {
    this.#require(id);
    const paused = this.#repos.automations.pause(id, null);
    if (paused === null) throw new AutomationHttpError('not_found', AUTOMATION_NOT_FOUND, 404);
    return serializeAutomation(paused);
  }

  resume(id: string): AutomationResource {
    const row = this.#require(id);
    const now = this.#now();

    let nextRunAt: number;
    if (row.type === 'periodic') {
      const schedule = this.#parseSchedule(row.expression ?? '', row.timezone ?? '');
      nextRunAt = this.#nextOccurrence(schedule, row.timezone ?? '', now);
    } else {
      const pending = this.#repos.automationInvocations
        .listByAutomation(id)
        .find((invocation) => invocation.status === 'queued');
      if (pending !== undefined && pending.scheduled_at !== null) {
        nextRunAt = pending.scheduled_at;
      } else if (row.run_at !== null && row.run_at > now) {
        nextRunAt = row.run_at;
      } else {
        throw new AutomationHttpError(
          'run_at_required',
          'A one-time automation needs a future run_at to resume',
          409,
          'run_at',
        );
      }
    }

    this.#repos.automations.setNextRunAt(id, nextRunAt);
    const resumed = this.#repos.automations.resume(id);
    if (resumed === null) throw new AutomationHttpError('not_found', AUTOMATION_NOT_FOUND, 404);
    return serializeAutomation(resumed);
  }

  remove(id: string): {
    readonly id: string;
    readonly object: 'agent.automation.deleted';
    readonly deleted: true;
  } {
    this.#require(id);
    this.#repos.automations.delete(id);
    return { id, object: 'agent.automation.deleted', deleted: true };
  }

  listInvocations(id: string, page: PageRequest): PlatformListPage<AutomationInvocationResource> {
    const automation = this.#require(id);
    const rows = this.#repos.automationInvocations.listByAutomation(id);
    const ordered = [...rows].sort(
      (left, right) => left.created_at - right.created_at || compareIds(left.id, right.id),
    );
    if (page.order === 'desc') ordered.reverse();
    const stored = paginate(ordered, page, (row) => row.id);
    return {
      ...stored,
      data: stored.data.map((row) => serializeInvocation(row, automation)),
    };
  }

  getInvocation(id: string, invocationId: string): AutomationInvocationResource {
    const automation = this.#require(id);
    const invocation = this.#repos.automationInvocations.get(invocationId);
    if (invocation === null || invocation.automation_id !== automation.id) {
      throw new AutomationHttpError('not_found', INVOCATION_NOT_FOUND, 404);
    }
    return serializeInvocation(invocation, automation);
  }

  /** A manual delivery is recorded as `manual` and queued; the saved schedule is untouched. */
  manualInvoke(id: string): AutomationInvocationResource {
    const automation = this.#require(id);
    const invocation = this.#repos.automationInvocations.create({
      automationId: id,
      source: 'manual',
      status: 'queued',
    });
    const resource = serializeInvocation(invocation, automation);
    this.#deferDelivery(automation, invocation);
    return resource;
  }

  /**
   * Waits until every deferred manual delivery settled. The wire contract answers `queued`, so
   * callers that need the final state wait here instead of polling stored rows.
   */
  async drain(): Promise<void> {
    while (this.#pending.size > 0) {
      await Promise.all([...this.#pending]);
    }
  }

  /** One scheduler tick: insert first (the unique schedule key makes it idempotent), then deliver. */
  async tick(): Promise<void> {
    const now = this.#now();
    const due = this.#repos.automations.listDue(now);
    for (const candidate of due) {
      try {
        await this.#processDue(candidate, now);
      } catch {
        // One corrupt schedule must not starve the rest of the due set.
        console.error('[automation] scheduled automation failed', { code: 'internal_error' });
      }
    }
  }

  async #processDue(candidate: PlatformAutomationRow, now: number): Promise<void> {
    const automation = this.#repos.automations.get(candidate.id);
    if (automation === null || automation.status !== 'active' || automation.next_run_at === null) {
      return;
    }
    const scheduledAt = automation.next_run_at;

    if (automation.type === 'periodic' && now - scheduledAt > this.#missedGraceMs) {
      // Missed while the process was stopped: no backfill, move to the first future occurrence.
      const schedule = this.#parseSchedule(automation.expression ?? '', automation.timezone ?? '');
      const nextRunAt = this.#nextOccurrence(schedule, automation.timezone ?? '', now);
      this.#repos.automations.setNextRunAt(automation.id, nextRunAt);
      return;
    }

    const existing = this.#repos.automationInvocations.getBySchedule(automation.id, scheduledAt);
    if (existing !== null) {
      // A surviving `queued` row means a previous process stopped before delivery; resolve it
      // instead of replaying work that may already have started.
      if (existing.status === 'queued') {
        this.#complete(existing.id, { status: 'skipped', skipReason: 'pending_delivery' });
      }
      this.#advanceSchedule(automation, scheduledAt);
      return;
    }

    const invocation = this.#repos.automationInvocations.create({
      automationId: automation.id,
      source: 'schedule',
      scheduledAt,
      status: 'queued',
    });
    this.#advanceSchedule(automation, scheduledAt);
    await this.#run(automation, invocation);
  }

  #advanceSchedule(automation: PlatformAutomationRow, scheduledAt: number): void {
    if (automation.type === 'periodic') {
      const schedule = this.#parseSchedule(automation.expression ?? '', automation.timezone ?? '');
      this.#repos.automations.setNextRunAt(
        automation.id,
        this.#nextOccurrence(schedule, automation.timezone ?? '', scheduledAt),
      );
      return;
    }
    this.#repos.automations.markCompleted(automation.id);
  }

  async #run(automation: PlatformAutomationRow, invocation: PlatformAutomationInvocationRow): Promise<void> {
    if (this.#running.has(automation.id)) {
      this.#complete(invocation.id, { status: 'skipped', skipReason: 'overlap' });
      return;
    }
    this.#running.add(automation.id);
    try {
      await this.#deliver(automation, invocation.id);
    } finally {
      this.#running.delete(automation.id);
    }
  }

  /** Starts a queued delivery outside the request that created it. */
  #deferDelivery(automation: PlatformAutomationRow, invocation: PlatformAutomationInvocationRow): void {
    let settle: () => void = () => {};
    const settled = new Promise<void>((resolve) => {
      settle = resolve;
    });
    this.#pending.add(settled);
    queueMicrotask(() => {
      void this.#run(automation, invocation)
        .catch(() => {
          console.error('[automation] manual invocation failed', { code: 'internal_error' });
        })
        .then(() => {
          this.#pending.delete(settled);
          settle();
        });
    });
  }

  async #deliver(automation: PlatformAutomationRow, invocationId: string): Promise<void> {
    let target: StoredAutomationTarget;
    try {
      target = parseStoredTarget(automation.target);
    } catch {
      this.#fail(invocationId, 'invalid_target', 'Automation target is invalid');
      return;
    }

    const port = this.#port;
    if (port === null) {
      this.#fail(invocationId, 'session_unavailable', 'Automation session runtime is unavailable');
      return;
    }

    if (target.type === 'existing_session') {
      const session = this.#repos.sessions.get(target.session_id);
      if (session === null) {
        this.#fail(invocationId, 'target_session_not_found', 'Target session not found');
        return;
      }
      if (session.status === 'failed') {
        // Only a periodic automation has a future occurrence that pausing protects. A one-time
        // automation stays completed; the delivery failure lives on its invocation.
        if (automation.type === 'periodic') {
          this.#repos.automations.pause(automation.id, 'target_session_failed');
          this.#complete(invocationId, {
            status: 'skipped',
            sessionId: target.session_id,
            skipReason: 'target_session_failed',
          });
        } else {
          this.#fail(invocationId, 'target_session_failed', 'Target session failed');
        }
        return;
      }
      try {
        await port.send(target.session_id, target.instructions);
        this.#complete(invocationId, { status: 'completed', sessionId: target.session_id });
      } catch {
        this.#fail(invocationId, 'delivery_failed', 'Automation delivery failed');
      }
      return;
    }

    try {
      const created = await port.create({
        environment: target.environment,
        input: target.input,
        ...(target.agent_id === undefined ? {} : { agent_id: target.agent_id }),
        ...(target.agent === undefined ? {} : { agent: target.agent }),
        ...(target.vault_ids === undefined ? {} : { vault_ids: target.vault_ids }),
        ...(target.metadata === undefined ? {} : { metadata: target.metadata }),
      });
      this.#complete(invocationId, { status: 'completed', sessionId: created.sessionId });
    } catch {
      this.#fail(invocationId, 'delivery_failed', 'Automation delivery failed');
    }
  }

  #complete(invocationId: string, input: CompletePlatformAutomationInvocation): void {
    this.#repos.automationInvocations.complete(invocationId, input);
  }

  #fail(invocationId: string, code: string, message: string): void {
    this.#repos.automationInvocations.complete(invocationId, {
      status: 'failed',
      errorCode: code,
      errorMessage: message,
    });
  }

  #require(id: string): PlatformAutomationRow {
    const row = this.#repos.automations.get(id);
    if (row === null) throw new AutomationHttpError('not_found', AUTOMATION_NOT_FOUND, 404);
    return row;
  }

  #validateTarget(target: AutomationTargetParam): void {
    if (target.type === 'existing_session') {
      if (this.#repos.sessions.get(target.session_id) === null) {
        throw new AutomationHttpError('not_found', 'Session not found', 404, 'target.session_id');
      }
      return;
    }
    if (target.environment === undefined) {
      throw invalid('target.environment is required', 'target.environment');
    }
    if (!Array.isArray(target.input)) {
      throw invalid('target.input must be an array of messages', 'target.input');
    }
    const hasAgent = target.agent !== undefined;
    const hasAgentId = target.agent_id !== undefined;
    if (hasAgent === hasAgentId) {
      throw invalid('Provide exactly one of target.agent and target.agent_id', 'target.agent_id');
    }
    if (target.agent_id !== undefined && this.#repos.agents.get(target.agent_id) === null) {
      throw new AutomationHttpError('not_found', 'Agent not found', 404, 'target.agent_id');
    }
  }

  #parseSchedule(expression: string, timezone: string): CronSchedule {
    if (!isSupportedTimeZone(timezone)) {
      throw new AutomationHttpError('timezone_unsupported', 'The timezone is not supported', 400, 'timezone');
    }
    try {
      return parseCron(expression);
    } catch {
      throw invalid('Invalid cron expression', 'expression');
    }
  }

  #nextOccurrence(schedule: CronSchedule, timezone: string, afterMs: number): number {
    try {
      return nextCronOccurrence(schedule, timezone, afterMs);
    } catch (error) {
      if (error instanceof CronTimeZoneError) {
        throw new AutomationHttpError(
          'timezone_unsupported',
          'The timezone is not supported',
          400,
          'timezone',
        );
      }
      if (error instanceof CronParseError) {
        throw invalid('Invalid cron expression', 'expression');
      }
      throw error;
    }
  }

  #nextAfter(expression: string, timezone: string, afterMs: number): number {
    return this.#nextOccurrence(this.#parseSchedule(expression, timezone), timezone, afterMs);
  }
}
