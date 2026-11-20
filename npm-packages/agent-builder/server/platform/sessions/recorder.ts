import type { ChatStreamHub } from '../../chats/stream';
import { diagnosticCode } from '../../errors';
import { parseJsonColumn } from '../json';
import type { PlatformRepositories } from '../repositories';
import type {
  PlatformEventRow,
  PlatformSessionRow,
  PlatformTurnRow,
  PlatformTurnStatus,
} from '../repositories/sessions';
import type { PlatformSubagentStatus } from '../repositories/subagents';
import {
  serializeSubagent,
  subagentTaskClosed,
  subagentTaskId,
  subagentTaskMillis,
  subagentTaskName,
} from '../subagents/serialize';
import {
  type ItemStatus,
  isLiveTurn,
  isTerminalTurn,
  mapTurnStatus,
  millisOf,
  type Snapshot,
  sumTurnUsage,
  type TurnState,
  type TurnView,
} from './diff';
import {
  newEventId,
  requiredActionsFromInteractions,
  type SessionEventData,
  type SubagentWire,
  sessionEvent,
  subagentEvent,
  turnEvent,
  turnTerminalEvent,
} from './events';
import { SessionItemWriter } from './item-writer';
import { serializeSession, serializeTurn, tokenUsage } from './serialize';

const TERMINAL_TURN_STATUSES: readonly PlatformTurnStatus[] = ['completed', 'failed', 'cancelled'];

interface SessionState {
  readonly sessionId: string;
  readonly agentId: string;
  status: PlatformSessionRow['status'];
  bindingRevision: number;
  requiredSignature: string;
  readonly turns: Map<string, TurnState>;
  /** Last status written for each subagent child thread observed in the parent projection. */
  readonly subagents: Map<string, PlatformSubagentStatus>;
  readonly subscribers: Set<(row: PlatformEventRow) => void>;
  readonly observed: Set<ChatStreamHub>;
}

export interface SessionRecorderOptions {
  readonly repos: PlatformRepositories;
  readonly now?: () => number;
}

/**
 * Turns the single chat projection into the durable session event log.
 *
 * The hub stays the only reducer: this recorder reads frozen snapshots and writes differences to
 * `platform_turns` / `platform_items` / `platform_events`. Text is diffed by prefix so a rewrite or
 * an overflow reset closes the current content part instead of emitting a negative delta.
 *
 * Adoption rules: only turns that are still live are announced. Terminal history and items that
 * already completed when the recorder attached are materialized silently, so reconnecting never
 * replays old events as new ones.
 */
export class SessionRecorder {
  readonly #repos: PlatformRepositories;
  readonly #now: () => number;
  readonly #sessions = new Map<string, SessionState>();
  readonly #items: SessionItemWriter;

  constructor(options: SessionRecorderOptions) {
    this.#repos = options.repos;
    this.#now = options.now ?? Date.now;
    this.#items = new SessionItemWriter({
      repos: this.#repos,
      now: this.#now,
      emit: (sessionId, data, refs) => this.#emitFor(sessionId, data, refs),
    });
  }

  /** Attaches to a session hub; idempotent per hub. Sessions without a row are ignored. */
  observe(hub: ChatStreamHub): () => void {
    const sessionId = hub.chatId;
    const state = this.#stateFor(sessionId);
    if (state === null) return () => {};
    if (state.observed.has(hub)) return () => void state.observed.delete(hub);

    state.observed.add(hub);
    this.#process(state, hub.snapshot, true);
    const off = hub.tap((snapshot) => this.#process(state, snapshot, false));
    return () => {
      off();
      state.observed.delete(hub);
    };
  }

  /** Live SSE subscription; the route replays first and dedupes by `seq`. */
  subscribe(sessionId: string, listener: (row: PlatformEventRow) => void): () => void {
    const state = this.#stateFor(sessionId);
    if (state === null) return () => {};
    state.subscribers.add(listener);
    return () => {
      state.subscribers.delete(listener);
    };
  }

  replay(sessionId: string, afterSeq: number): PlatformEventRow[] {
    return this.#repos.events.listBySession(sessionId, afterSeq);
  }

  /** Persists and broadcasts `agent.session.created` for a freshly inserted row. */
  recordSessionCreated(sessionId: string): void {
    const row = this.#repos.sessions.get(sessionId);
    if (row === null) return;
    const eventRow = this.#append(
      sessionId,
      sessionEvent('agent.session.created', newEventId(), this.#sessionResource(row)),
      {},
    );
    const state = this.#sessions.get(sessionId);
    if (state !== undefined) for (const listener of [...state.subscribers]) listener(eventRow);
  }

  /** Resolves a public `event_id` back to its log position for `Last-Event-ID` replay. */
  sequenceOf(sessionId: string, eventId: string): number | null {
    return this.#repos.events.findByEventId(sessionId, eventId)?.seq ?? null;
  }

  /**
   * The chat run was confirmed closed by the caller (not by the projection): no turn is running.
   * Cached turns are dropped so the next attach re-hydrates them from storage.
   */
  resetRun(sessionId: string): void {
    const row = this.#repos.sessions.get(sessionId);
    if (row === null) return;
    const state = this.#sessions.get(sessionId);
    if (state !== undefined) {
      state.turns.clear();
      state.requiredSignature = '[]';
    }
    const wasActive = row.status === 'in_progress' || row.status === 'requires_action';
    this.#repos.sessions.setStatus(sessionId, 'idle', null);
    this.#repos.sessions.update(sessionId, {
      agentConfig: null,
      environmentType: null,
      environmentId: null,
      metadata: null,
      vaultIds: null,
      requiredActions: '[]',
      usage: null,
      promptCacheOptions: null,
      review: null,
      environmentResetCount: null,
    });
    if (state !== undefined) state.status = 'idle';
    if (!wasActive) return;
    const updated = this.#repos.sessions.get(sessionId) ?? row;
    this.#append(
      sessionId,
      sessionEvent('agent.session.idle', newEventId(), this.#sessionResource(updated)),
      {},
    );
  }

  /** Drops cached session state after the platform row is gone. */
  forget(sessionId: string): void {
    this.#sessions.delete(sessionId);
  }

  /** Appends a subagent transition; the projection has no subagent channel yet. */
  recordSubagent(
    sessionId: string,
    type:
      | 'agent.session.subagent.created'
      | 'agent.session.subagent.active'
      | 'agent.session.subagent.closed',
    subagent: SubagentWire,
  ): void {
    const row = this.#append(sessionId, subagentEvent(type, newEventId(), subagent), {});
    const state = this.#sessions.get(sessionId);
    if (state !== undefined) for (const listener of [...state.subscribers]) listener(row);
  }

  /**
   * Startup convergence: nothing can still be running after a restart. Non-terminal turns become
   * `failed/interrupted`, non-terminal items become `incomplete`, and the events are appended
   * without broadcasting (no subscriber can exist yet).
   */
  sweepInterrupted(): { turns: number; items: number } {
    const now = this.#now();
    const sessions = new Set<string>();
    let turns = 0;
    for (const turn of this.#repos.turns.listNonTerminal()) {
      this.#repos.turns.complete(turn.id, 'failed', {
        completedAt: now,
        errorCode: 'interrupted',
        errorMessage: 'Server restarted while the turn was running',
      });
      const updated = this.#repos.turns.get(turn.id);
      if (updated === null) continue;
      this.#append(
        turn.session_id,
        turnTerminalEvent('agent.session.turn.failed', newEventId(), serializeTurn(updated), null),
        { turnId: turn.id },
      );
      sessions.add(turn.session_id);
      turns += 1;
    }

    let items = 0;
    for (const item of this.#repos.items.listNonTerminal()) {
      const payload = parseJsonColumn<Record<string, unknown>>(item.payload, {});
      this.#repos.items.update(item.id, {
        status: 'incomplete',
        payload: JSON.stringify({ ...payload, status: 'incomplete' }),
        completedAt: now,
      });
      items += 1;
    }

    for (const sessionId of sessions) {
      this.#repos.sessions.setStatus(sessionId, 'failed', 'interrupted');
      this.#repos.sessions.update(sessionId, {
        agentConfig: null,
        environmentType: null,
        environmentId: null,
        metadata: null,
        vaultIds: null,
        requiredActions: '[]',
        usage: null,
        promptCacheOptions: null,
        review: null,
        environmentResetCount: null,
      });
      const row = this.#repos.sessions.get(sessionId);
      if (row === null) continue;
      this.#append(
        sessionId,
        sessionEvent('agent.session.failed', newEventId(), this.#sessionResource(row)),
        {},
      );
      const state = this.#sessions.get(sessionId);
      if (state !== undefined) state.status = 'failed';
    }

    return { turns, items };
  }

  #stateFor(sessionId: string): SessionState | null {
    const existing = this.#sessions.get(sessionId);
    if (existing !== undefined) return existing;
    const row = this.#repos.sessions.get(sessionId);
    if (row === null) return null;
    const created: SessionState = {
      sessionId,
      agentId: row.agent_id,
      status: row.status,
      bindingRevision: 0,
      requiredSignature: row.required_actions,
      turns: new Map(),
      subagents: new Map(
        this.#repos.subagents.listBySession(sessionId).map((subagent) => [subagent.id, subagent.status]),
      ),
      subscribers: new Set(),
      observed: new Set(),
    };
    this.#sessions.set(sessionId, created);
    return created;
  }

  #process(state: SessionState, snapshot: Snapshot, seed: boolean): void {
    try {
      this.#diff(state, snapshot, seed);
    } catch (error) {
      // A recorder fault must never tear down the hub's own projection loop.
      console.error('[agent-builder] session recorder failed', { code: diagnosticCode(error) });
    }
  }

  #diff(state: SessionState, snapshot: Snapshot, seed: boolean): void {
    if (snapshot.bindingRevision !== null) state.bindingRevision = snapshot.bindingRevision;

    const views = new Map<string, TurnView>();
    for (const view of snapshot.turns) views.set(view.turnId, view);

    this.#syncStatus(state, snapshot);
    for (const view of snapshot.turns) this.#syncTurn(state, view);

    const live = new Set<string>();
    for (const entry of snapshot.entries) {
      live.add(`entry:${entry.entryId}`);
      const turn = this.#ensureTurnForItem(state, entry.turnId, views.get(entry.turnId) ?? null);
      if (turn !== null) this.#items.syncEntry(state.sessionId, turn, entry, seed);
    }
    for (const tool of snapshot.toolCalls) {
      live.add(`tool:${tool.toolCallId}`);
      const turn = this.#ensureTurnForItem(state, tool.turnId ?? '', views.get(tool.turnId ?? '') ?? null);
      if (turn !== null) this.#items.syncTool(state.sessionId, turn, tool, seed);
    }

    for (const turn of state.turns.values()) {
      for (const [key, item] of turn.items) {
        if (!live.has(key) && !item.done) {
          this.#items.finish(state.sessionId, turn, item, 'incomplete');
        }
      }
    }
    for (const view of snapshot.turns) {
      const turn = state.turns.get(view.turnId);
      if (turn !== undefined) this.#finishTurn(state, turn, view);
    }

    // Subagents are a separate projection channel; a failure here must never stop turn/item sync.
    try {
      this.#syncSubagents(state, snapshot.tasks);
    } catch (error) {
      console.error('[agent-builder] session subagent projection failed', {
        code: diagnosticCode(error),
      });
    }
  }

  #syncStatus(state: SessionState, snapshot: Snapshot): void {
    const liveTurn = snapshot.turns.some((view) => isLiveTurn(view));
    const next: PlatformSessionRow['status'] =
      snapshot.pendingInteractions.length > 0
        ? 'requires_action'
        : snapshot.running || liveTurn
          ? 'in_progress'
          : snapshot.closed
            ? 'failed'
            : 'idle';
    const required = JSON.stringify(requiredActionsFromInteractions(snapshot.pendingInteractions));
    if (next === state.status && required === state.requiredSignature) return;

    const statusChanged = next !== state.status;
    state.status = next;
    state.requiredSignature = required;
    this.#repos.sessions.setStatus(
      state.sessionId,
      next,
      snapshot.closed ? (snapshot.closedReason?.code ?? 'closed') : null,
    );
    this.#repos.sessions.update(state.sessionId, {
      agentConfig: null,
      environmentType: null,
      environmentId: null,
      metadata: null,
      vaultIds: null,
      requiredActions: required,
      usage: null,
      promptCacheOptions: null,
      review: null,
      environmentResetCount: null,
    });
    if (!statusChanged) return;
    const row = this.#repos.sessions.get(state.sessionId);
    if (row === null) return;
    const type =
      next === 'idle'
        ? 'agent.session.idle'
        : next === 'in_progress'
          ? 'agent.session.in_progress'
          : next === 'requires_action'
            ? 'agent.session.requires_action'
            : 'agent.session.failed';
    this.#emit(state, sessionEvent(type, newEventId(), this.#sessionResource(row)), {});
  }

  /**
   * Adopts subagent tasks from the parent projection. A live subagent is announced as created and
   * active; a terminal one closes the stored row. Rows first seen after they already finished are
   * written silently, matching the recorder's adoption rule for terminal turns.
   */
  #syncSubagents(state: SessionState, tasks: Snapshot['tasks']): void {
    for (const task of tasks) {
      const id = subagentTaskId(task);
      if (id === null) continue;
      const closed = subagentTaskClosed(task);
      const known = state.subagents.get(id);
      if (known === undefined) {
        const existing = this.#repos.subagents.get(id);
        if (existing !== null) {
          if (existing.session_id === state.sessionId) state.subagents.set(id, existing.status);
          continue;
        }
        const row = this.#repos.subagents.create({
          id,
          sessionId: state.sessionId,
          name: subagentTaskName(task),
          parentAgentId: state.agentId,
          status: closed ? 'closed' : 'active',
          openedAt: subagentTaskMillis(task.startedAt) ?? this.#now(),
          ...(closed ? { closedAt: subagentTaskMillis(task.completedAt) ?? this.#now() } : {}),
        });
        state.subagents.set(id, row.status);
        if (closed) continue;
        const wire = serializeSubagent(row);
        this.recordSubagent(state.sessionId, 'agent.session.subagent.created', wire);
        this.recordSubagent(state.sessionId, 'agent.session.subagent.active', wire);
        continue;
      }
      if (known !== 'active' || !closed) continue;
      const row = this.#repos.subagents.close(id, subagentTaskMillis(task.completedAt) ?? this.#now());
      state.subagents.set(id, 'closed');
      if (row !== null) {
        this.recordSubagent(state.sessionId, 'agent.session.subagent.closed', serializeSubagent(row));
      }
    }
  }

  #syncTurn(state: SessionState, view: TurnView): TurnState | null {
    const existing = state.turns.get(view.turnId);
    if (existing !== undefined) {
      if (!existing.announced && !existing.terminal && view.startedAt !== null && !isTerminalTurn(view)) {
        this.#announceTurn(state, existing, view);
      }
      return existing;
    }
    const row = this.#repos.turns.findLatestByRuntime(state.sessionId, view.turnId);
    if (row !== null) {
      const turn = this.#hydrateTurn(row);
      state.turns.set(view.turnId, turn);
      return turn;
    }
    if (view.startedAt === null) return null;

    const terminal = isTerminalTurn(view);
    const created = this.#repos.turns.create({
      sessionId: state.sessionId,
      agentId: state.agentId,
      runtimeTurnId: view.turnId,
      bindingRevision: state.bindingRevision,
      status: terminal ? mapTurnStatus(view.status) : 'in_progress',
      startedAt: millisOf(view.startedAt) ?? this.#now(),
    });
    const turn = this.#hydrateTurn(created);
    state.turns.set(view.turnId, turn);
    if (terminal) turn.announced = true;
    else this.#announceTurn(state, turn, view);
    return turn;
  }

  #announceTurn(state: SessionState, turn: TurnState, view: TurnView): void {
    turn.announced = true;
    turn.status = 'in_progress';
    this.#repos.turns.setStatus(turn.id, 'in_progress', millisOf(view.startedAt));
    const row = this.#repos.turns.get(turn.id);
    if (row === null) return;
    const resource = serializeTurn(row);
    this.#emit(state, turnEvent('agent.session.turn.created', newEventId(), resource), { turnId: turn.id });
    this.#emit(state, turnEvent('agent.session.turn.in_progress', newEventId(), resource), {
      turnId: turn.id,
    });
  }

  #hydrateTurn(row: PlatformTurnRow): TurnState {
    const turn: TurnState = {
      id: row.id,
      runtimeTurnId: row.runtime_turn_id,
      status: row.status,
      announced: true,
      terminal: TERMINAL_TURN_STATUSES.includes(row.status),
      nextOutputIndex: 0,
      items: new Map(),
      entryUsage: new Map(),
    };
    this.#items.hydrate(turn);
    return turn;
  }

  #finishTurn(state: SessionState, turn: TurnState, view: TurnView): void {
    if (turn.terminal) return;
    const status = mapTurnStatus(view.status);
    if (!isTerminalTurn(view)) return;
    const usage = sumTurnUsage(turn);
    const error = view.error === null ? null : { code: view.error.code, message: view.error.message };
    const fallback: ItemStatus = status === 'completed' ? 'completed' : 'incomplete';
    for (const item of turn.items.values()) this.#items.finish(state.sessionId, turn, item, fallback);
    this.#repos.turns.complete(turn.id, status, {
      completedAt: millisOf(view.completedAt) ?? this.#now(),
      stopReason: view.stopReason ?? null,
      errorCode: error?.code ?? null,
      errorMessage: error?.message ?? null,
      usage: usage === null ? null : JSON.stringify(usage),
    });
    turn.terminal = true;
    turn.status = status;
    const row = this.#repos.turns.get(turn.id);
    if (row !== null) {
      const type =
        status === 'completed'
          ? 'agent.session.turn.completed'
          : status === 'cancelled'
            ? 'agent.session.turn.cancelled'
            : 'agent.session.turn.failed';
      this.#emit(state, turnTerminalEvent(type, newEventId(), serializeTurn(row), usage), {
        turnId: turn.id,
      });
    }
    this.#persistSessionUsage(state);
  }

  #persistSessionUsage(state: SessionState): void {
    let input = 0;
    let output = 0;
    let seen = false;
    for (const turn of this.#repos.turns.listBySession(state.sessionId)) {
      if (turn.usage === null) continue;
      const usage = parseJsonColumn<{ input_tokens?: number; output_tokens?: number }>(turn.usage, {});
      input += usage.input_tokens ?? 0;
      output += usage.output_tokens ?? 0;
      seen = true;
    }
    if (!seen) return;
    this.#repos.sessions.update(state.sessionId, {
      agentConfig: null,
      environmentType: null,
      environmentId: null,
      metadata: null,
      vaultIds: null,
      requiredActions: null,
      usage: JSON.stringify(tokenUsage({ inputTokens: input, outputTokens: output })),
      promptCacheOptions: null,
      review: null,
      environmentResetCount: null,
    });
  }

  #ensureTurnForItem(state: SessionState, runtimeTurnId: string, view: TurnView | null): TurnState | null {
    if (runtimeTurnId.length === 0) return null;
    const existing = state.turns.get(runtimeTurnId);
    if (existing !== undefined) return existing;
    if (view !== null) {
      const synced = this.#syncTurn(state, view);
      if (synced !== null) return synced;
    }
    const row = this.#repos.turns.create({
      sessionId: state.sessionId,
      agentId: state.agentId,
      runtimeTurnId,
      bindingRevision: state.bindingRevision,
      status: 'queued',
    });
    const turn = this.#hydrateTurn(row);
    turn.announced = false;
    state.turns.set(runtimeTurnId, turn);
    return turn;
  }

  #emit(
    state: SessionState,
    data: SessionEventData,
    refs: { turnId?: string | null; itemId?: string | null },
  ): void {
    this.#emitFor(state.sessionId, data, refs);
  }

  #emitFor(
    sessionId: string,
    data: SessionEventData,
    refs: { turnId?: string | null; itemId?: string | null },
  ): void {
    const row = this.#append(sessionId, data, {
      turnId: refs.turnId ?? null,
      itemId: refs.itemId ?? null,
    });
    const state = this.#sessions.get(sessionId);
    if (state !== undefined) for (const listener of [...state.subscribers]) listener(row);
  }

  #append(
    sessionId: string,
    data: SessionEventData,
    refs: { turnId?: string | null; itemId?: string | null },
  ): PlatformEventRow {
    return this.#repos.events.append({
      sessionId,
      // The column and the payload must carry the same public id: clients echo the payload
      // `event_id` as `Last-Event-ID`, and replay resolves it through the column.
      eventId: data.event_id,
      type: data.type,
      turnId: refs.turnId ?? null,
      itemId: refs.itemId ?? null,
      data: JSON.stringify(data),
    });
  }

  #sessionResource(row: PlatformSessionRow) {
    return serializeSession(row, this.#repos.agents, this.#repos.environments);
  }
}
