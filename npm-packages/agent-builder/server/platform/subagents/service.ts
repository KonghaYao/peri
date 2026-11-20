import { type PageRequest, type PlatformListPage, paginate } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import type { PlatformSessionRow } from '../repositories/sessions';
import type { PlatformSubagentRow } from '../repositories/subagents';
import { SessionHttpError } from '../sessions/errors';
import {
  type SessionItemWire,
  serializeItem,
  serializeTurn,
  type TurnResourceWire,
} from '../sessions/serialize';
import {
  type StoreHistoryTurn,
  StoreHistoryUnavailableError,
  StoreSchemaUnsupportedError,
  type StoreThreadHistory,
  type SubagentHistoryReader,
  subagentHistoryReaderFor,
} from './history';
import { type SubagentResourceWire, serializeSubagent } from './serialize';

export const SUBAGENT_HISTORY_DEGRADED = 'subagent_history_unavailable';

/** The empty page used whenever history must degrade instead of inventing rows. */
export function emptyPage<T>(): PlatformListPage<T> {
  return { object: 'list', data: [], first_id: null, last_id: null, has_more: false };
}

function ordered<T>(rows: readonly T[], order: PageRequest['order']): readonly T[] {
  return order === 'asc' ? rows : [...rows].reverse();
}

interface TurnAttribution {
  readonly sessionId: string;
  readonly subagentId: string;
  readonly agentId: string;
}

/**
 * Applies the documented subagent operations to the platform tables and, when installed, the
 * child-thread Store reader. Turn/item identity comes from exactly one source per request: the
 * Store wins when a reader is installed, otherwise the recorder-owned platform tables answer.
 */
export class SubagentService {
  readonly #repos: PlatformRepositories;

  constructor(repos: PlatformRepositories) {
    this.#repos = repos;
  }

  requireSession(sessionId: string): PlatformSessionRow {
    const row = this.#repos.sessions.get(sessionId);
    if (row === null) throw new SessionHttpError('not_found', 'Session not found', 404);
    return row;
  }

  requireSubagent(sessionId: string, subagentId: string): PlatformSubagentRow {
    this.requireSession(sessionId);
    const row = this.#repos.subagents.get(subagentId);
    if (row === null || row.session_id !== sessionId) {
      throw new SessionHttpError('not_found', 'Subagent not found', 404);
    }
    return row;
  }

  list(sessionId: string, request: PageRequest): PlatformListPage<SubagentResourceWire> {
    this.requireSession(sessionId);
    const rows = ordered(this.#repos.subagents.listBySession(sessionId), request.order);
    const stored = paginate(rows, request, (row) => row.id);
    return { ...stored, data: stored.data.map(serializeSubagent) };
  }

  get(sessionId: string, subagentId: string): SubagentResourceWire {
    return serializeSubagent(this.requireSubagent(sessionId, subagentId));
  }

  async turns(
    sessionId: string,
    subagentId: string,
    request: PageRequest,
  ): Promise<PlatformListPage<TurnResourceWire>> {
    const subagent = this.requireSubagent(sessionId, subagentId);
    const reader = this.#reader();
    if (reader === null) {
      const rows = ordered(this.#repos.turns.listBySubagent(sessionId, subagentId), request.order);
      const stored = paginate(rows, request, (row) => row.id);
      return { ...stored, data: stored.data.map(serializeTurn) };
    }
    const history = await readStore(reader, subagentId);
    const turns = history.turns.map((turn) => turnWire(turn, attribution(subagent)));
    return paginate(ordered(turns, request.order), request, (turn) => turn.id);
  }

  async turn(sessionId: string, subagentId: string, turnId: string): Promise<TurnResourceWire> {
    const subagent = this.requireSubagent(sessionId, subagentId);
    const reader = this.#reader();
    if (reader === null) {
      const row = this.#repos.turns.get(turnId);
      if (row === null || row.session_id !== sessionId || row.subagent_id !== subagentId) {
        throw new SessionHttpError('not_found', 'Subagent turn not found', 404);
      }
      return serializeTurn(row);
    }
    const history = await readStore(reader, subagentId);
    const turn = history.turns.find((candidate) => candidate.id === turnId);
    if (turn === undefined) throw new SessionHttpError('not_found', 'Subagent turn not found', 404);
    return turnWire(turn, attribution(subagent));
  }

  async items(
    sessionId: string,
    subagentId: string,
    request: PageRequest,
  ): Promise<PlatformListPage<SessionItemWire>> {
    this.requireSubagent(sessionId, subagentId);
    const reader = this.#reader();
    if (reader === null) {
      const rows = ordered(this.#repos.items.listBySubagent(sessionId, subagentId), request.order);
      const stored = paginate(rows, request, (row) => row.id);
      return { ...stored, data: stored.data.map(serializeItem) };
    }
    const history = await readStore(reader, subagentId);
    const items = history.turns.flatMap((turn) => turn.items.map((item) => ({ ...item, turn_id: turn.id })));
    return paginate(ordered(items, request.order), request, (item) => item.id);
  }

  async turnItems(
    sessionId: string,
    subagentId: string,
    turnId: string,
    request: PageRequest,
  ): Promise<PlatformListPage<SessionItemWire>> {
    this.requireSubagent(sessionId, subagentId);
    const reader = this.#reader();
    if (reader === null) {
      const row = this.#repos.turns.get(turnId);
      if (row === null || row.session_id !== sessionId || row.subagent_id !== subagentId) {
        throw new SessionHttpError('not_found', 'Subagent turn not found', 404);
      }
      const rows = ordered(this.#repos.items.listByTurn(turnId), request.order);
      const stored = paginate(rows, request, (item) => item.id);
      return { ...stored, data: stored.data.map(serializeItem) };
    }
    const history = await readStore(reader, subagentId);
    const turn = history.turns.find((candidate) => candidate.id === turnId);
    if (turn === undefined) throw new SessionHttpError('not_found', 'Subagent turn not found', 404);
    const items = turn.items.map((item) => ({ ...item, turn_id: turn.id }));
    return paginate(ordered(items, request.order), request, (item) => item.id);
  }

  #reader(): SubagentHistoryReader | null {
    return subagentHistoryReaderFor(this.#repos.subagents.db);
  }
}

function attribution(subagent: PlatformSubagentRow): TurnAttribution {
  return {
    sessionId: subagent.session_id,
    subagentId: subagent.id,
    agentId: subagent.parent_agent_id,
  };
}

function turnWire(turn: StoreHistoryTurn, attribution: TurnAttribution): TurnResourceWire {
  return {
    object: 'agent.session.turn',
    id: turn.id,
    session_id: attribution.sessionId,
    agent_id: attribution.agentId,
    subagent_id: attribution.subagentId,
    status: turn.status,
    created_at: turn.created_at,
    started_at: turn.started_at,
    completed_at: turn.completed_at,
    error: null,
    usage: null,
    prompt_cache_options: null,
  };
}

/** Probe first so an incompatible Store is a 409 instead of a silent empty page. */
async function readStore(reader: SubagentHistoryReader, threadId: string): Promise<StoreThreadHistory> {
  try {
    await reader.probe();
  } catch (error) {
    throw normalizeStoreError(error);
  }
  try {
    return await reader.read(threadId);
  } catch (error) {
    throw normalizeStoreError(error);
  }
}

function normalizeStoreError(error: unknown): Error {
  if (error instanceof StoreSchemaUnsupportedError) return error;
  if (error instanceof StoreHistoryUnavailableError) return error;
  return new StoreHistoryUnavailableError('Subagent history could not be read from the Store');
}
