import type { Database } from 'bun:sqlite';
import { newPlatformId } from '../ids';
import type { ListQuery } from './shared';
import { buildListStatement, nowMillis, removeById } from './shared';

export type PlatformEnvironmentType = 'none' | 'self_hosted' | 'local';
export type PlatformEnvironmentStatus = 'pending' | 'ready' | 'connected' | 'disconnected' | 'failed';

export interface PlatformEnvironmentRow {
  readonly id: string;
  readonly type: PlatformEnvironmentType;
  readonly status: PlatformEnvironmentStatus;
  readonly name: string;
  readonly config: string;
  readonly created_at: number;
  readonly updated_at: number;
}

export interface NewPlatformEnvironment {
  readonly id?: string;
  readonly type: PlatformEnvironmentType;
  readonly status?: PlatformEnvironmentStatus;
  readonly name: string;
  readonly config: string;
}

export interface PlatformEnvironmentUpdate {
  readonly type: PlatformEnvironmentType | null;
  readonly status: PlatformEnvironmentStatus | null;
  readonly name: string | null;
  readonly config: string | null;
}

const ENVIRONMENT_COLUMNS = 'id, type, status, name, config, created_at, updated_at';

export interface PlatformEnvironmentsRepositoryOptions {
  readonly now?: () => number;
}

export class PlatformEnvironmentsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformEnvironmentsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  list(query: ListQuery, types: readonly PlatformEnvironmentType[] | null = null): PlatformEnvironmentRow[] {
    if (types === null) {
      const { sql, params } = buildListStatement('platform_environments', ENVIRONMENT_COLUMNS, query);
      return this.db.query<PlatformEnvironmentRow, (string | number)[]>(sql).all(...params);
    }
    const direction = query.order === 'asc' ? 'ASC' : 'DESC';
    const params: (string | number)[] = [...types];
    let where = `WHERE type IN (${types.map(() => '?').join(', ')})`;
    if (query.after !== null) {
      where += ` AND id ${query.order === 'asc' ? '>' : '<'} ?`;
      params.push(query.after);
    }
    params.push(query.limit);
    return this.db
      .query<PlatformEnvironmentRow, (string | number)[]>(
        `SELECT ${ENVIRONMENT_COLUMNS} FROM platform_environments ${where} ORDER BY id ${direction} LIMIT ?`,
      )
      .all(...params);
  }

  get(id: string): PlatformEnvironmentRow | null {
    const row = this.db
      .query<PlatformEnvironmentRow, [string]>(
        `SELECT ${ENVIRONMENT_COLUMNS} FROM platform_environments WHERE id = ?`,
      )
      .get(id);
    return row ?? null;
  }

  create(input: NewPlatformEnvironment): PlatformEnvironmentRow {
    const id = input.id ?? newPlatformId();
    const now = nowMillis(this.#now);
    this.db
      .query(
        `INSERT INTO platform_environments (id, type, status, name, config, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(id, input.type, input.status ?? 'pending', input.name, input.config, now, now);
    const row = this.get(id);
    if (!row) throw new Error(`Created environment ${id} could not be read back`);
    return row;
  }

  update(id: string, input: PlatformEnvironmentUpdate): PlatformEnvironmentRow | null {
    this.db
      .query(
        `UPDATE platform_environments SET
          type = COALESCE(?, type),
          status = COALESCE(?, status),
          name = COALESCE(?, name),
          config = COALESCE(?, config),
          updated_at = ?
        WHERE id = ?`,
      )
      .run(input.type, input.status, input.name, input.config, nowMillis(this.#now), id);
    return this.get(id);
  }

  delete(id: string): boolean {
    return removeById(this.db, 'platform_environments', id);
  }
}

export interface PlatformEnvironmentTemplateRow {
  readonly id: string;
  readonly name: string;
  readonly config: string;
  readonly created_at: number;
  readonly updated_at: number;
}

export interface NewPlatformEnvironmentTemplate {
  readonly id?: string;
  readonly name: string;
  readonly config: string;
}

export interface PlatformEnvironmentTemplateUpdate {
  readonly name: string | null;
  readonly config: string | null;
}

const TEMPLATE_COLUMNS = 'id, name, config, created_at, updated_at';

export class PlatformEnvironmentTemplatesRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformEnvironmentsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  list(query: ListQuery): PlatformEnvironmentTemplateRow[] {
    const { sql, params } = buildListStatement('platform_environment_templates', TEMPLATE_COLUMNS, query);
    return this.db.query<PlatformEnvironmentTemplateRow, (string | number)[]>(sql).all(...params);
  }

  get(id: string): PlatformEnvironmentTemplateRow | null {
    const row = this.db
      .query<PlatformEnvironmentTemplateRow, [string]>(
        `SELECT ${TEMPLATE_COLUMNS} FROM platform_environment_templates WHERE id = ?`,
      )
      .get(id);
    return row ?? null;
  }

  create(input: NewPlatformEnvironmentTemplate): PlatformEnvironmentTemplateRow {
    const id = input.id ?? newPlatformId();
    const now = nowMillis(this.#now);
    this.db
      .query(
        `INSERT INTO platform_environment_templates (id, name, config, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)`,
      )
      .run(id, input.name, input.config, now, now);
    const row = this.get(id);
    if (!row) throw new Error(`Created environment template ${id} could not be read back`);
    return row;
  }

  update(id: string, input: PlatformEnvironmentTemplateUpdate): PlatformEnvironmentTemplateRow | null {
    this.db
      .query(
        `UPDATE platform_environment_templates SET
          name = COALESCE(?, name),
          config = COALESCE(?, config),
          updated_at = ?
        WHERE id = ?`,
      )
      .run(input.name, input.config, nowMillis(this.#now), id);
    return this.get(id);
  }

  delete(id: string): boolean {
    return removeById(this.db, 'platform_environment_templates', id);
  }
}

export interface PlatformEnvironmentFileRow {
  readonly id: string;
  readonly environment_id: string;
  readonly path: string;
  readonly content_ciphertext: Uint8Array;
  readonly size_bytes: number;
  readonly created_at: number;
}

export interface NewPlatformEnvironmentFile {
  readonly id?: string;
  readonly environmentId: string;
  readonly path: string;
  /** Sealed payload produced by the platform encryption layer; never plaintext. */
  readonly contentCiphertext: Uint8Array;
  readonly sizeBytes: number;
}

const FILE_COLUMNS = 'id, environment_id, path, content_ciphertext, size_bytes, created_at';

export class PlatformEnvironmentFilesRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformEnvironmentsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  listByEnvironment(environmentId: string): PlatformEnvironmentFileRow[] {
    return this.db
      .query<PlatformEnvironmentFileRow, [string]>(
        `SELECT ${FILE_COLUMNS} FROM platform_environment_files
         WHERE environment_id = ? ORDER BY path ASC`,
      )
      .all(environmentId);
  }

  get(id: string): PlatformEnvironmentFileRow | null {
    const row = this.db
      .query<PlatformEnvironmentFileRow, [string]>(
        `SELECT ${FILE_COLUMNS} FROM platform_environment_files WHERE id = ?`,
      )
      .get(id);
    return row ?? null;
  }

  getByPath(environmentId: string, path: string): PlatformEnvironmentFileRow | null {
    const row = this.db
      .query<PlatformEnvironmentFileRow, [string, string]>(
        `SELECT ${FILE_COLUMNS} FROM platform_environment_files
         WHERE environment_id = ? AND path = ?`,
      )
      .get(environmentId, path);
    return row ?? null;
  }

  create(input: NewPlatformEnvironmentFile): PlatformEnvironmentFileRow {
    const id = input.id ?? newPlatformId();
    this.db
      .query(
        `INSERT INTO platform_environment_files (
          id, environment_id, path, content_ciphertext, size_bytes, created_at
        ) VALUES (?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.environmentId,
        input.path,
        input.contentCiphertext,
        input.sizeBytes,
        nowMillis(this.#now),
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created environment file ${id} could not be read back`);
    return row;
  }

  updateContent(
    id: string,
    contentCiphertext: Uint8Array,
    sizeBytes: number,
  ): PlatformEnvironmentFileRow | null {
    this.db
      .query(
        `UPDATE platform_environment_files
         SET content_ciphertext = ?, size_bytes = ?
         WHERE id = ?`,
      )
      .run(contentCiphertext, sizeBytes, id);
    return this.get(id);
  }

  delete(id: string): boolean {
    return removeById(this.db, 'platform_environment_files', id);
  }
}
