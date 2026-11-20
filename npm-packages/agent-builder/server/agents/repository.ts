import type { Database } from 'bun:sqlite';
import {
  type Agent,
  AgentConfig,
  type AgentDraft,
  type AgentSummary,
  isReferenceProvider,
} from '@shared/agent';
import { AppError } from '../errors';
import { isHiddenAgentId } from '../platform/agents/visibility';

interface AgentRow {
  readonly id: string;
  readonly name: string;
  readonly description: string;
  readonly config: string;
  readonly created_at: number;
  readonly updated_at: number;
  readonly provider_id: string | null;
  readonly revision: number;
}

export class AgentHasChatsError extends Error {
  readonly agentId: string;
  constructor(agentId: string) {
    super(`Agent ${agentId} still has chats`);
    this.agentId = agentId;
    this.name = 'AgentHasChatsError';
  }
}

export interface AgentsRepositoryOptions {
  readonly now?: () => number;
  readonly generateId?: () => string;
}

const SELECT_COLUMNS = 'id, name, description, config, created_at, updated_at, provider_id, revision';

// provider_id 是唯一引用权威；JSON 中只保存每 Agent 的 model。
export function parseStoredConfig(agentId: string, raw: string, providerId: string | null): AgentConfig {
  try {
    const config = JSON.parse(raw);
    if (providerId !== null) {
      if (!config.provider || Object.keys(config.provider).length !== 1 || !('model' in config.provider))
        throw new Error('Invalid stored reference');
      config.provider = { kind: 'reference', providerId, model: config.provider.model };
    } else if (config.provider && 'kind' in config.provider) {
      throw new Error('Reference requires provider_id');
    }
    return AgentConfig.parse(config);
  } catch {
    throw new Error(`Stored agent config is invalid for agent ${agentId}`);
  }
}

function storedConfig(draft: AgentDraft): { config: string; providerId: string | null } {
  const provider = draft.config.provider;
  const reference = provider !== null && isReferenceProvider(provider);
  return {
    config: JSON.stringify({ ...draft.config, provider: reference ? { model: provider.model } : provider }),
    providerId: reference ? provider.providerId : null,
  };
}

function toSummary(row: AgentRow): AgentSummary {
  return {
    id: row.id,
    name: row.name,
    description: row.description,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    revision: row.revision,
  };
}
function toAgent(row: AgentRow): Agent {
  return { ...toSummary(row), config: parseStoredConfig(row.id, row.config, row.provider_id) };
}

export class AgentsRepository {
  readonly #now: () => number;
  readonly #generateId: () => string;
  readonly db: Database;
  constructor(db: Database, options: AgentsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
    this.#generateId = options.generateId ?? (() => crypto.randomUUID());
  }

  list(): AgentSummary[] {
    return (
      this.db
        .query<AgentRow, []>(`SELECT ${SELECT_COLUMNS} FROM agents ORDER BY updated_at DESC, id ASC`)
        .all()
        // Reserved rows back internal sessions; they are not user-created agents.
        .filter((row) => !isHiddenAgentId(row.id))
        .map(toSummary)
    );
  }
  get(agentId: string): Agent | null {
    if (isHiddenAgentId(agentId)) return null;
    const row = this.db
      .query<AgentRow, [string]>(`SELECT ${SELECT_COLUMNS} FROM agents WHERE id = ?`)
      .get(agentId);
    return row ? toAgent(row) : null;
  }

  #assertProvider(id: string | null): void {
    if (id !== null && !this.db.query('SELECT id FROM providers WHERE id = ?').get(id))
      throw new AppError('not_found', 'Referenced Provider not found');
  }

  create(draft: AgentDraft): Agent {
    return this.db
      .transaction(() => {
        const now = this.#now();
        const id = this.#generateId();
        const stored = storedConfig(draft);
        this.#assertProvider(stored.providerId);
        this.db
          .query(`INSERT INTO agents (id, name, description, config, created_at, updated_at, provider_id)
        VALUES (?, ?, ?, ?, ?, ?, ?)`)
          .run(id, draft.name, draft.description, stored.config, now, now, stored.providerId);
        return { id, ...structuredClone(draft), createdAt: now, updatedAt: now, revision: 1 };
      })
      .immediate();
  }

  update(agentId: string, draft: AgentDraft, revision?: number): Agent | null {
    return this.db
      .transaction(() => {
        const existing = this.get(agentId);
        if (!existing) return null;
        if (existing.revision !== revision) throw new AppError('revision_conflict', 'Stale Agent revision');
        const stored = storedConfig(draft);
        this.#assertProvider(stored.providerId);
        const updatedAt = this.#now();
        const result = this.db
          .query(
            'UPDATE agents SET name = ?, description = ?, config = ?, provider_id = ?, updated_at = ?, revision = revision + 1 WHERE id = ? AND revision = ?',
          )
          .run(draft.name, draft.description, stored.config, stored.providerId, updatedAt, agentId, revision);
        if (result.changes !== 1) throw new AppError('revision_conflict', 'Stale Agent revision');
        return { ...existing, ...structuredClone(draft), updatedAt, revision: revision + 1 };
      })
      .immediate();
  }

  delete(agentId: string, revision?: number): boolean {
    return this.db
      .transaction(() => {
        const existing = this.get(agentId);
        if (!existing) return false;
        if (existing.revision !== revision) throw new AppError('revision_conflict', 'Stale Agent revision');
        if (this.db.query('SELECT 1 FROM chats WHERE agent_id = ? LIMIT 1').get(agentId))
          throw new AgentHasChatsError(agentId);
        if (
          this.db.query('DELETE FROM agents WHERE id = ? AND revision = ?').run(agentId, revision).changes !==
          1
        )
          throw new AppError('revision_conflict', 'Stale Agent revision');
        return true;
      })
      .immediate();
  }
}
