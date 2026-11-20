import type { Database } from 'bun:sqlite';
import { isHiddenAgentId } from '../agents/visibility';
import { newPlatformId } from '../ids';
import { EMPTY_JSON_ARRAY, EMPTY_JSON_OBJECT } from '../json';
import type { PageOrder } from '../pagination';
import type { ListQuery } from './shared';
import { buildListStatement, removeById } from './shared';

/**
 * `agents` is the shared table created by migration v1; platform agents are ordinary rows in it.
 * Timestamps stay in Unix milliseconds and responses convert them at the serializer boundary.
 */
export interface PlatformAgentRow {
  readonly id: string;
  readonly name: string;
  /** Full documented name when it exceeds the 80-character `name` column bound. */
  readonly wire_name: string | null;
  readonly description: string;
  readonly config: string;
  readonly provider_id: string | null;
  readonly model: string | null;
  readonly metadata: string;
  readonly tools: string;
  readonly multi_agent: string | null;
  readonly reasoning: string | null;
  readonly text_config: string | null;
  readonly service_tier: string | null;
  readonly compaction: string | null;
  readonly experimental: string | null;
  readonly created_at: number;
  readonly updated_at: number;
}

export interface NewPlatformAgent {
  readonly id?: string;
  readonly name: string;
  readonly wireName?: string | null;
  readonly description?: string;
  readonly config?: string;
  readonly providerId?: string | null;
  readonly model?: string | null;
  readonly metadata?: string;
  readonly tools?: string;
  readonly multiAgent?: string | null;
  readonly reasoning?: string | null;
  readonly textConfig?: string | null;
  readonly serviceTier?: string | null;
  readonly compaction?: string | null;
  readonly experimental?: string | null;
}

export type AgentDeleteOutcome = 'deleted' | 'missing' | 'referenced';

const COLUMNS =
  'id, name, wire_name, description, config, provider_id, model, metadata, tools, multi_agent, reasoning, text_config, service_tier, compaction, experimental, created_at, updated_at';

export interface PlatformAgentsRepositoryOptions {
  readonly now?: () => number;
}

export class PlatformAgentsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformAgentsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  list(query: ListQuery): PlatformAgentRow[] {
    const { sql, params } = buildListStatement('agents', COLUMNS, query);
    return this.db.query<PlatformAgentRow, (string | number)[]>(sql).all(...params);
  }

  /**
   * Lists rows the platform API exposes: the shared table without the reserved inline-session
   * agent. Ordered by ID so a cursor page stays stable while `after` continues the scan.
   */
  listVisible(order: PageOrder): PlatformAgentRow[] {
    const direction = order === 'asc' ? 'ASC' : 'DESC';
    return this.db
      .query<PlatformAgentRow, []>(`SELECT ${COLUMNS} FROM agents ORDER BY id ${direction}`)
      .all()
      .filter((row) => !isHiddenAgentId(row.id));
  }

  get(id: string): PlatformAgentRow | null {
    const row = this.db
      .query<PlatformAgentRow, [string]>(`SELECT ${COLUMNS} FROM agents WHERE id = ?`)
      .get(id);
    return row ?? null;
  }

  create(input: NewPlatformAgent): PlatformAgentRow {
    const id = input.id ?? newPlatformId();
    const now = this.#now();
    this.db
      .query(
        `INSERT INTO agents (
          id, name, wire_name, description, config, provider_id, model, metadata, tools, multi_agent,
          reasoning, text_config, service_tier, compaction, experimental, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.name,
        input.wireName ?? null,
        input.description ?? '',
        input.config ?? EMPTY_JSON_OBJECT,
        input.providerId ?? null,
        input.model ?? null,
        input.metadata ?? EMPTY_JSON_OBJECT,
        input.tools ?? EMPTY_JSON_ARRAY,
        input.multiAgent ?? null,
        input.reasoning ?? null,
        input.textConfig ?? null,
        input.serviceTier ?? null,
        input.compaction ?? null,
        input.experimental ?? null,
        now,
        now,
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created agent ${id} could not be read back`);
    return row;
  }

  /** Replaces every writable column; `id`, `created_at` and `revision` are server-owned. */
  update(id: string, input: NewPlatformAgent): PlatformAgentRow | null {
    const changes = this.db
      .query(
        `UPDATE agents SET
          name = ?,
          wire_name = ?,
          description = ?,
          config = ?,
          provider_id = ?,
          model = ?,
          metadata = ?,
          tools = ?,
          multi_agent = ?,
          reasoning = ?,
          text_config = ?,
          service_tier = ?,
          compaction = ?,
          experimental = ?,
          updated_at = ?,
          revision = revision + 1
        WHERE id = ?`,
      )
      .run(
        input.name,
        input.wireName ?? null,
        input.description ?? '',
        input.config ?? EMPTY_JSON_OBJECT,
        input.providerId ?? null,
        input.model ?? null,
        input.metadata ?? EMPTY_JSON_OBJECT,
        input.tools ?? EMPTY_JSON_ARRAY,
        input.multiAgent ?? null,
        input.reasoning ?? null,
        input.textConfig ?? null,
        input.serviceTier ?? null,
        input.compaction ?? null,
        input.experimental ?? null,
        this.#now(),
        id,
      );
    return changes.changes === 0 ? null : this.get(id);
  }

  delete(id: string): boolean {
    return removeById(this.db, 'agents', id);
  }

  /**
   * Deletes an agent unless legacy chats or platform sessions still reference it. Both foreign
   * keys are RESTRICT, so checking first turns an opaque constraint failure into a 409.
   */
  deleteIfUnreferenced(id: string): AgentDeleteOutcome {
    return this.db
      .transaction((): AgentDeleteOutcome => {
        if (isHiddenAgentId(id) || this.get(id) === null) return 'missing';
        const chat = this.db
          .query<{ present: number }, [string]>('SELECT 1 AS present FROM chats WHERE agent_id = ? LIMIT 1')
          .get(id);
        const session = this.db
          .query<{ present: number }, [string]>(
            'SELECT 1 AS present FROM platform_sessions WHERE agent_id = ? LIMIT 1',
          )
          .get(id);
        if (chat !== null || session !== null) return 'referenced';
        return this.delete(id) ? 'deleted' : 'missing';
      })
      .immediate();
  }
}
