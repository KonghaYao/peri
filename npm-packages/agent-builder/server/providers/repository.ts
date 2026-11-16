import type { Database } from 'bun:sqlite';
import type { ProviderType } from '@shared/provider';

export interface ProviderRecord {
  readonly id: string;
  readonly name: string;
  readonly type: ProviderType;
  readonly baseUrl: string | null;
  readonly credentialSource: 'env' | 'local';
  readonly apiKey: string | null;
  readonly revision: number;
}

interface ProviderRow {
  id: string;
  name: string;
  type: ProviderType;
  base_url: string | null;
  credential_source: 'env' | 'local';
  api_key: string | null;
  revision: number;
}
const COLUMNS = 'id, name, type, base_url, credential_source, api_key, revision';
function fromRow(row: ProviderRow): ProviderRecord {
  return {
    id: row.id,
    name: row.name,
    type: row.type,
    baseUrl: row.base_url,
    credentialSource: row.credential_source,
    apiKey: row.api_key,
    revision: row.revision,
  };
}

export class ProvidersRepository {
  readonly db: Database;
  constructor(db: Database) {
    this.db = db;
  }

  transaction<T>(operation: () => T): T {
    return this.db.transaction(operation).immediate();
  }

  list(): ProviderRecord[] {
    return this.db
      .query<ProviderRow, []>(`SELECT ${COLUMNS} FROM providers ORDER BY name, id`)
      .all()
      .map(fromRow);
  }

  get(id: string): ProviderRecord | null {
    const row = this.db.query<ProviderRow, [string]>(`SELECT ${COLUMNS} FROM providers WHERE id = ?`).get(id);
    return row ? fromRow(row) : null;
  }

  create(record: ProviderRecord): void {
    this.db
      .query(
        'INSERT INTO providers (id, name, type, base_url, credential_source, api_key, revision) VALUES (?, ?, ?, ?, ?, ?, ?)',
      )
      .run(
        record.id,
        record.name,
        record.type,
        record.baseUrl,
        record.credentialSource,
        record.apiKey,
        record.revision,
      );
  }

  update(record: ProviderRecord, revision: number): boolean {
    return (
      this.db
        .query(
          'UPDATE providers SET name = ?, base_url = ?, credential_source = ?, api_key = ?, revision = revision + 1 WHERE id = ? AND revision = ?',
        )
        .run(record.name, record.baseUrl, record.credentialSource, record.apiKey, record.id, revision)
        .changes === 1
    );
  }

  delete(id: string, revision: number): boolean {
    return (
      this.db.query('DELETE FROM providers WHERE id = ? AND revision = ?').run(id, revision).changes === 1
    );
  }

  agentCount(id: string): number {
    return (
      this.db
        .query<{ count: number }, [string]>('SELECT count(*) AS count FROM agents WHERE provider_id = ?')
        .get(id)?.count ?? 0
    );
  }
}
