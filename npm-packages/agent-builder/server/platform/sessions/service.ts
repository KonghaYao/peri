import { createHash } from 'node:crypto';
import type { InteractionAnswer } from '@peri-code/sdk';
import { SEND_TEXT_MAX_LENGTH } from '@shared/chat';
import type { ChatEnvironmentPolicy } from '../../chats/service';
import type { ChatStreamHub } from '../../chats/stream';
import { newPlatformId } from '../ids';
import { type PageRequest, type PlatformListPage, paginate } from '../pagination';
import type { PlatformEnvironmentType } from '../repositories/environments';
import type { PlatformSessionRow, PlatformTurnRow } from '../repositories/sessions';
import { patchSessionAgent, resolveSessionAgent } from './agent-config';
import { SessionHttpError } from './errors';
import {
  type CreateSessionEventsParams,
  type CreateSessionParams,
  inputText,
  messageInputText,
  type SessionEventInput,
  type UpdateSessionParams,
} from './params';
import type { SessionRecorder } from './recorder';
import type { SessionRuntime } from './runtime';
import {
  readAgentSnapshot,
  type SessionItemWire,
  type SessionResourceWire,
  serializeAgentSnapshot,
  serializeItem,
  serializeSession,
  serializeTurn,
  type TurnResourceWire,
} from './serialize';
import { openSessionEventTap, resumePosition, type SessionEventSource, type SessionEventTap } from './sse';

export interface SessionListFilter {
  readonly agentId?: string;
  readonly metadata?: Record<string, string>;
}

/** Reasons reported in `x-platform-degraded` when a requested capability ran degraded. */
export const ENVIRONMENT_TYPE_MAPPED_REASON = 'environment_type_mapped_to_local';

const LOCAL_ENVIRONMENT_TYPES: ReadonlySet<string> = new Set(['none', 'self_hosted', 'local']);

/** Unknown hosting types run in the local sandbox instead of failing the DDL check. */
function normalizeEnvironmentType(requested: string): PlatformEnvironmentType {
  return LOCAL_ENVIRONMENT_TYPES.has(requested) ? (requested as PlatformEnvironmentType) : 'local';
}

export interface CreatedSession {
  readonly session: SessionResourceWire;
  readonly hub: ChatStreamHub;
  /** Present when `stream:true`; it was subscribed before the initial input was submitted. */
  readonly tap: SessionEventTap | null;
  readonly degradation: readonly string[];
}

/**
 * Applies the documented session operations to the runtime's chat service and recorder.
 *
 * Execution goes through `ChatService` only: this layer never talks to the actor directly, so the
 * per-chat queue, sandbox holder and provider retention keep their single owners.
 */
export class SessionService {
  readonly #runtime: SessionRuntime;
  readonly #recorder: SessionRecorder;

  constructor(runtime: SessionRuntime) {
    this.#runtime = runtime;
    this.#recorder = runtime.recorder;
  }

  get eventSource(): SessionEventSource & Pick<SessionRecorder, 'sequenceOf'> {
    return {
      replay: (sessionId, afterSeq) => this.#recorder.replay(sessionId, afterSeq),
      subscribe: (sessionId, listener) => this.#recorder.subscribe(sessionId, listener),
      sequenceOf: (sessionId, eventId) => this.#recorder.sequenceOf(sessionId, eventId),
    };
  }

  require(sessionId: string): PlatformSessionRow {
    const row = this.#runtime.repos.sessions.get(sessionId);
    if (row === null) throw new SessionHttpError('not_found', 'Session not found', 404);
    return row;
  }

  resource(row: PlatformSessionRow): SessionResourceWire {
    return serializeSession(row, this.#runtime.repos.agents, this.#runtime.repos.environments);
  }

  /** Degradations that apply to a stored session resource; routes surface them as headers. */
  degradation(row: PlatformSessionRow): string[] {
    return row.environment_type === 'local' ? [ENVIRONMENT_TYPE_MAPPED_REASON] : [];
  }

  async create(
    params: CreateSessionParams,
    options: { readonly signal?: AbortSignal | undefined } = {},
  ): Promise<CreatedSession> {
    const { repos, chats, recorder } = this.#runtime;
    const environmentType = normalizeEnvironmentType(params.environment.type);
    const degradation: string[] = environmentType === 'local' ? [ENVIRONMENT_TYPE_MAPPED_REASON] : [];
    if (params.stream !== true && params.stream_options !== undefined) {
      throw new SessionHttpError(
        'invalid_input',
        'stream_options requires stream=true',
        400,
        'stream_options',
      );
    }
    const text = inputText(params.input ?? null);
    if (environmentType === 'none' && text === null) {
      throw new SessionHttpError(
        'invalid_input',
        'input is required when environment.type is none',
        400,
        'input',
      );
    }
    if (text !== null) this.#assertSendableText(text);
    this.#assertVaults(params.vault_ids ?? []);

    const resolved = resolveSessionAgent({
      agents: repos.agents,
      providers: chats.providers,
      ...(params.agent_id === undefined ? {} : { agentId: params.agent_id }),
      ...(params.agent === undefined ? {} : { agent: params.agent }),
    });
    const sessionId = newPlatformId();
    const policy: ChatEnvironmentPolicy = {
      holderFor: () => (environmentType === 'none' ? null : sessionId),
    };

    repos.sessions.create({
      id: sessionId,
      agentId: resolved.agentId,
      agentConfig: serializeAgentSnapshot(resolved.snapshot),
      environmentType,
      environmentId: null,
      metadata: JSON.stringify(params.metadata ?? {}),
      vaultIds: JSON.stringify(params.vault_ids ?? []),
      status: 'idle',
      requiredActions: '[]',
      promptCacheOptions:
        params.prompt_cache_options === undefined || params.prompt_cache_options === null
          ? null
          : JSON.stringify(params.prompt_cache_options),
      review: params.review ?? null,
    });

    try {
      await chats.create(
        { agentId: resolved.agentId },
        { chatId: sessionId, draft: resolved.snapshot.draft, policy },
      );
    } catch (error) {
      repos.sessions.delete(sessionId);
      throw error;
    }

    recorder.recordSessionCreated(sessionId);
    let hub: ChatStreamHub;
    try {
      hub = await chats.prepareStream(sessionId);
    } catch (error) {
      await this.#discard(sessionId);
      throw error;
    }
    recorder.observe(hub);
    // Subscribe the SSE tap before the first input: nothing produced between creation and the
    // first byte can be lost, and a failed submit still returns a JSON error.
    const tap =
      params.stream === true
        ? openSessionEventTap(this.eventSource, sessionId, 0, options.signal ?? new AbortController().signal)
        : null;

    if (text !== null) {
      try {
        await chats.send(sessionId, newPlatformId(), text);
      } catch (error) {
        tap?.dispose();
        await this.#discard(sessionId);
        throw error;
      }
    }

    return { session: this.resource(this.require(sessionId)), hub, tap, degradation };
  }

  async update(sessionId: string, params: UpdateSessionParams): Promise<SessionResourceWire> {
    const { repos, chats } = this.#runtime;
    const row = this.require(sessionId);
    if (params.vault_ids !== undefined) this.#assertVaults(params.vault_ids ?? []);

    let agentConfig: string | null = null;
    if (params.agent !== undefined) {
      const snapshot = readAgentSnapshot(row.agent_config);
      if (snapshot === null) {
        throw new SessionHttpError(
          'invalid_input',
          'This session has no agent snapshot to update',
          400,
          'agent',
        );
      }
      agentConfig = serializeAgentSnapshot(patchSessionAgent(snapshot, params.agent));
    }

    repos.sessions.update(sessionId, {
      agentConfig,
      environmentType: null,
      environmentId: null,
      metadata: params.metadata === undefined ? null : JSON.stringify(params.metadata ?? {}),
      vaultIds: params.vault_ids === undefined ? null : JSON.stringify(params.vault_ids ?? []),
      requiredActions: null,
      usage: null,
      promptCacheOptions:
        params.prompt_cache_options === undefined
          ? null
          : params.prompt_cache_options === null
            ? 'null'
            : JSON.stringify(params.prompt_cache_options),
      review: params.review ?? null,
      environmentResetCount: null,
    });

    if (agentConfig !== null) {
      // Model settings apply to subsequent turns: drop the frozen run so the next send re-freezes
      // from the stored snapshot. A confirmed close means no turn is running any more.
      await chats.close(sessionId);
      this.#recorder.resetRun(sessionId);
    }
    return this.resource(this.require(sessionId));
  }

  async remove(sessionId: string): Promise<{ id: string; object: 'agent.session.deleted'; deleted: true }> {
    const { repos, chats } = this.#runtime;
    const row = this.require(sessionId);
    if (row.status === 'in_progress' || row.status === 'requires_action') {
      throw new SessionHttpError(
        'session_active',
        'Cancel the running turn before deleting the session',
        409,
      );
    }
    try {
      await chats.remove(sessionId);
    } catch (error) {
      if (!(typeof error === 'object' && error !== null && 'code' in error && error.code === 'not_found')) {
        throw error;
      }
    }
    repos.sessions.delete(sessionId);
    this.#recorder.forget(sessionId);
    return { id: sessionId, object: 'agent.session.deleted', deleted: true };
  }

  list(page: PageRequest, filter: SessionListFilter): PlatformListPage<SessionResourceWire> {
    const rows = this.#runtime.repos.sessions.list(
      { limit: page.limit + 1, order: page.order, after: page.after },
      { agentId: filter.agentId ?? null, metadata: filter.metadata ?? {} },
    );
    const visible = rows.slice(0, page.limit);
    // The repository already applied the keyset cursor; `paginate` must not apply it twice.
    const stored = paginate(visible, { limit: page.limit, order: page.order, after: null }, (row) => row.id);
    return {
      ...stored,
      has_more: rows.length > page.limit,
      data: stored.data.map((row) => this.resource(row)),
    };
  }

  items(sessionId: string, page: PageRequest): PlatformListPage<SessionItemWire> {
    this.require(sessionId);
    const rows = ordered(this.#runtime.repos.items.listBySession(sessionId), page);
    const stored = paginate(rows, page, (row) => row.id);
    return { ...stored, data: stored.data.map(serializeItem) };
  }

  turns(sessionId: string, page: PageRequest): PlatformListPage<TurnResourceWire> {
    this.require(sessionId);
    const rows = ordered(this.#runtime.repos.turns.listBySession(sessionId), page);
    const stored = paginate(rows, page, (row) => row.id);
    return { ...stored, data: stored.data.map(serializeTurn) };
  }

  turn(sessionId: string, turnId: string): TurnResourceWire {
    const row = this.#turnOf(sessionId, turnId);
    return serializeTurn(row);
  }

  turnItems(sessionId: string, turnId: string, page: PageRequest): PlatformListPage<SessionItemWire> {
    this.#turnOf(sessionId, turnId);
    const rows = ordered(this.#runtime.repos.items.listByTurn(turnId), page);
    const stored = paginate(rows, page, (row) => row.id);
    return { ...stored, data: stored.data.map(serializeItem) };
  }

  /** Attaches the projection and opens the SSE tap; throws before any byte is committed. */
  async stream(
    sessionId: string,
    lastEventId: string | undefined,
    signal: AbortSignal,
  ): Promise<SessionEventTap> {
    this.require(sessionId);
    const hub = await this.#runtime.chats.prepareStream(sessionId);
    this.#recorder.observe(hub);
    const after = resumePosition(this.eventSource, sessionId, lastEventId);
    return openSessionEventTap(this.eventSource, sessionId, after, signal);
  }

  /** `POST .../events`: validated inputs, durable idempotency, then the chat actions. */
  async submitInputs(
    sessionId: string,
    body: CreateSessionEventsParams,
    idempotencyKey: string | null,
  ): Promise<void> {
    this.require(sessionId);
    const digest = createHash('sha256').update(JSON.stringify(body)).digest('hex');
    if (idempotencyKey !== null) {
      const existing = this.#runtime.receipts.get(sessionId, idempotencyKey);
      if (existing !== null) {
        if (existing.digest === digest) return;
        throw new SessionHttpError(
          'idempotency_conflict',
          'Idempotency-Key was already used with a different body',
          409,
          'Idempotency-Key',
        );
      }
    }
    if (body.events.length === 0) return;
    this.#assertInputs(body.events);

    const requestId = newPlatformId();
    if (idempotencyKey !== null) {
      try {
        this.#runtime.receipts.insert({ sessionId, idempotencyKey, digest, requestId });
      } catch {
        // A concurrent retry won the insert; its receipt is the recorded outcome.
        const raced = this.#runtime.receipts.get(sessionId, idempotencyKey);
        if (raced?.digest === digest) return;
        throw new SessionHttpError(
          'idempotency_conflict',
          'Idempotency-Key was already used with a different body',
          409,
          'Idempotency-Key',
        );
      }
    }
    try {
      const hub = await this.#runtime.chats.prepareStream(sessionId);
      this.#recorder.observe(hub);
      await this.#apply(sessionId, body.events, requestId, hub);
    } catch (error) {
      if (idempotencyKey !== null) this.#runtime.receipts.delete(sessionId, idempotencyKey);
      throw error;
    }
  }

  async #apply(
    sessionId: string,
    events: readonly SessionEventInput[],
    requestId: string,
    hub: ChatStreamHub,
  ): Promise<void> {
    let index = 0;
    for (const event of events) {
      const actionRequestId = `${requestId}:${index}`;
      index += 1;
      if (event.type === 'agent.session.input.message') {
        await this.#runtime.chats.send(sessionId, actionRequestId, messageInputText(event));
        continue;
      }
      if (event.type === 'agent.session.input.cancel') {
        await this.#runtime.chats.cancel(sessionId, actionRequestId);
        continue;
      }
      const interaction = hub.pendingInteractions.find((pending) => pending.id === event.request_id);
      if (interaction === undefined) {
        throw new SessionHttpError('not_found', 'Pending interaction not found', 404, 'request_id');
      }
      await this.#runtime.chats.answer(
        sessionId,
        interaction.id,
        answerFor(event.response.decision, interaction.kind),
      );
    }
  }

  #assertInputs(events: readonly SessionEventInput[]): void {
    for (const event of events) {
      if (event.type !== 'agent.session.input.message') continue;
      this.#assertSendableText(messageInputText(event));
    }
  }

  #assertSendableText(text: string): void {
    if (text.trim().length === 0) {
      throw new SessionHttpError('invalid_input', 'Input text cannot be blank', 400, 'input');
    }
    if (text.length > SEND_TEXT_MAX_LENGTH) {
      throw new SessionHttpError('invalid_input', 'Input text is too long', 400, 'input');
    }
  }

  #assertVaults(vaultIds: readonly string[]): void {
    for (const vaultId of vaultIds) {
      if (this.#runtime.repos.vaults.get(vaultId) === null) {
        throw new SessionHttpError('invalid_input', 'Unknown vault in vault_ids', 400, 'vault_ids');
      }
    }
  }

  #turnOf(sessionId: string, turnId: string): PlatformTurnRow {
    this.require(sessionId);
    const row = this.#runtime.repos.turns.get(turnId);
    if (row === null || row.session_id !== sessionId) {
      throw new SessionHttpError('not_found', 'Turn not found', 404);
    }
    return row;
  }

  /** Removes a session whose chat could not be prepared; the event log cascades with the row. */
  async #discard(sessionId: string): Promise<void> {
    try {
      await this.#runtime.chats.remove(sessionId);
    } catch {
      // The chat row may already be gone; the platform row is the one that must not linger.
    }
    this.#runtime.repos.sessions.delete(sessionId);
  }
}

function answerFor(decision: 'approve' | 'deny' | 'cancel', kind: string): InteractionAnswer {
  if (kind === 'permission') {
    return { kind: 'permission', decision: decision === 'approve' ? 'allow_once' : 'reject_once' };
  }
  return {
    kind: 'elicitation',
    decision:
      decision === 'approve'
        ? { action: 'accept', content: {} }
        : { action: decision === 'deny' ? 'decline' : 'cancel' },
  };
}

function ordered<T>(rows: readonly T[], page: PageRequest): T[] {
  return page.order === 'asc' ? [...rows] : [...rows].reverse();
}
