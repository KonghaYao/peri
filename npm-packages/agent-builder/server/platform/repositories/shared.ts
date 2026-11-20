import type { Database } from 'bun:sqlite';
import type { PageOrder } from '../pagination';

/** Keyset page input shared by the platform repositories. */
export interface ListQuery {
  readonly limit: number;
  readonly order: PageOrder;
  readonly after: string | null;
}

export interface ListStatement {
  readonly sql: string;
  readonly params: readonly (string | number)[];
}

/**
 * Builds a keyset query over the resource `id` column.
 * Table and column names are internal constants, never request input.
 */
export function buildListStatement(table: string, columns: string, query: ListQuery): ListStatement {
  const direction = query.order === 'asc' ? 'ASC' : 'DESC';
  if (query.after === null) {
    return {
      sql: `SELECT ${columns} FROM ${table} ORDER BY id ${direction} LIMIT ?`,
      params: [query.limit],
    };
  }
  const comparison = query.order === 'asc' ? '>' : '<';
  return {
    sql: `SELECT ${columns} FROM ${table} WHERE id ${comparison} ? ORDER BY id ${direction} LIMIT ?`,
    params: [query.after, query.limit],
  };
}

export function removeById(db: Database, table: string, id: string): boolean {
  return db.query(`DELETE FROM ${table} WHERE id = ?`).run(id).changes > 0;
}

/** Platform tables persist Unix milliseconds; API serializers convert to seconds. */
export function nowMillis(now: () => number): number {
  return now();
}
