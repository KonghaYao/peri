import type { Database } from 'bun:sqlite';
import { newPlatformId } from '../ids';
import { nowMillis, removeById } from './shared';

export interface PlatformArtifactRow {
  readonly id: string;
  readonly session_id: string;
  /** Null when the artifact was produced outside a hosted environment or publishing turn. */
  readonly environment_id: string | null;
  readonly turn_id: string | null;
  readonly path: string;
  readonly media_type: string | null;
  readonly size_bytes: number;
  readonly created_at: number;
}

export interface NewPlatformArtifact {
  readonly id?: string;
  readonly sessionId: string;
  readonly environmentId?: string | null;
  readonly turnId?: string | null;
  readonly path: string;
  readonly mediaType?: string | null;
  readonly sizeBytes: number;
}

const ARTIFACT_COLUMNS = 'id, session_id, environment_id, turn_id, path, media_type, size_bytes, created_at';

export interface PlatformArtifactsRepositoryOptions {
  readonly now?: () => number;
}

export class PlatformArtifactsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformArtifactsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  listBySession(sessionId: string): PlatformArtifactRow[] {
    return this.db
      .query<PlatformArtifactRow, [string]>(
        `SELECT ${ARTIFACT_COLUMNS} FROM platform_artifacts
         WHERE session_id = ? ORDER BY created_at ASC, id ASC`,
      )
      .all(sessionId);
  }

  get(id: string): PlatformArtifactRow | null {
    const row = this.db
      .query<PlatformArtifactRow, [string]>(`SELECT ${ARTIFACT_COLUMNS} FROM platform_artifacts WHERE id = ?`)
      .get(id);
    return row ?? null;
  }

  create(input: NewPlatformArtifact): PlatformArtifactRow {
    const id = input.id ?? newPlatformId();
    this.db
      .query(
        `INSERT INTO platform_artifacts (
           id, session_id, environment_id, turn_id, path, media_type, size_bytes, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.sessionId,
        input.environmentId ?? null,
        input.turnId ?? null,
        input.path,
        input.mediaType ?? null,
        input.sizeBytes,
        nowMillis(this.#now),
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created artifact ${id} could not be read back`);
    return row;
  }

  delete(id: string): boolean {
    return removeById(this.db, 'platform_artifacts', id);
  }
}
