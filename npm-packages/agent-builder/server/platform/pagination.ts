import { PlatformRequestError } from './errors';

export const DEFAULT_PAGE_LIMIT = 20;
export const MAX_PAGE_LIMIT = 100;
/** Matches the documented maximum length of an `after` cursor. */
export const MAX_CURSOR_LENGTH = 1_048_576;

export type PageOrder = 'asc' | 'desc';

export interface PageRequest {
  readonly limit: number;
  readonly order: PageOrder;
  /** Resource ID to continue after, or null for the first page. */
  readonly after: string | null;
}

export interface PageQuery {
  readonly limit?: string | undefined;
  readonly order?: string | undefined;
  readonly after?: string | undefined;
}

export interface PlatformListPage<T> {
  readonly object: 'list';
  readonly data: readonly T[];
  readonly first_id: string | null;
  readonly last_id: string | null;
  readonly has_more: boolean;
}

function parseLimit(raw: string | undefined): number {
  if (raw === undefined || raw.length === 0) return DEFAULT_PAGE_LIMIT;
  if (!/^\d+$/.test(raw)) {
    throw new PlatformRequestError('limit must be an integer between 1 and 100', 'limit');
  }
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new PlatformRequestError('limit must be an integer between 1 and 100', 'limit');
  }
  return Math.min(value, MAX_PAGE_LIMIT);
}

function parseOrder(raw: string | undefined): PageOrder {
  if (raw === undefined || raw.length === 0) return 'desc';
  if (raw === 'asc' || raw === 'desc') return raw;
  throw new PlatformRequestError('order must be either "asc" or "desc"', 'order');
}

/** Cursors are resource IDs in the platform protocol; encoding is the identity function. */
export function encodeCursor(id: string): string {
  return id;
}

export function decodeCursor(raw: string | null | undefined): string | null {
  if (raw === null || raw === undefined) return null;
  const trimmed = raw.trim();
  if (trimmed.length === 0) return null;
  if (trimmed.length > MAX_CURSOR_LENGTH) {
    throw new PlatformRequestError('after cursor is too long', 'after');
  }
  return trimmed;
}

export function parsePageRequest(query: PageQuery): PageRequest {
  return {
    limit: parseLimit(query.limit),
    order: parseOrder(query.order),
    after: decodeCursor(query.after),
  };
}

/**
 * Applies `after` and `limit` to an already ordered page of rows.
 * An unknown cursor yields an empty page, mirroring a keyset query past the end.
 */
export function paginate<T>(
  items: readonly T[],
  request: PageRequest,
  idOf: (item: T) => string,
): PlatformListPage<T> {
  let start = 0;
  if (request.after !== null) {
    const index = items.findIndex((item) => idOf(item) === request.after);
    start = index === -1 ? items.length : index + 1;
  }
  const data = items.slice(start, start + request.limit);
  const first = data[0];
  const last = data[data.length - 1];
  return {
    object: 'list',
    data,
    first_id: first === undefined ? null : idOf(first),
    last_id: last === undefined ? null : idOf(last),
    has_more: start + request.limit < items.length,
  };
}
