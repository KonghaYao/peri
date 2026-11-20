import { describe, expect, test } from 'bun:test';
import { PlatformRequestError } from '../../../server/platform/errors';
import {
  DEFAULT_PAGE_LIMIT,
  decodeCursor,
  encodeCursor,
  MAX_PAGE_LIMIT,
  paginate,
  parsePageRequest,
} from '../../../server/platform/pagination';

const ITEMS = [{ id: 'a' }, { id: 'b' }, { id: 'c' }, { id: 'd' }, { id: 'e' }] as const;

function paramOf(error: unknown): string | null | undefined {
  return error instanceof PlatformRequestError ? error.param : undefined;
}

describe('parsePageRequest', () => {
  test('applies documented defaults', () => {
    expect(parsePageRequest({})).toEqual({
      limit: DEFAULT_PAGE_LIMIT,
      order: 'desc',
      after: null,
    });
  });

  test('clamps the limit to the documented maximum', () => {
    expect(parsePageRequest({ limit: '500' }).limit).toBe(MAX_PAGE_LIMIT);
    expect(parsePageRequest({ limit: '1' }).limit).toBe(1);
  });

  test.each(['0', '-1', 'abc', '1.5'])('rejects limit %s', (limit) => {
    expect(() => parsePageRequest({ limit })).toThrow(PlatformRequestError);
    try {
      parsePageRequest({ limit });
    } catch (error) {
      expect(paramOf(error)).toBe('limit');
    }
  });

  test('accepts both orders and rejects anything else', () => {
    expect(parsePageRequest({ order: 'asc' }).order).toBe('asc');
    expect(parsePageRequest({ order: 'desc' }).order).toBe('desc');
    try {
      parsePageRequest({ order: 'random' });
    } catch (error) {
      expect(paramOf(error)).toBe('order');
    }
  });

  test('treats a blank after value as the first page', () => {
    expect(parsePageRequest({ after: '' }).after).toBeNull();
    expect(parsePageRequest({ after: '   ' }).after).toBeNull();
    expect(parsePageRequest({ after: ' cursor-1 ' }).after).toBe('cursor-1');
  });
});

describe('cursors', () => {
  test('round-trips a resource ID', () => {
    expect(decodeCursor(encodeCursor('cursor-abc'))).toBe('cursor-abc');
    expect(decodeCursor(undefined)).toBeNull();
  });
});

describe('paginate', () => {
  test('returns the page after the cursor and reports more rows', () => {
    const page = paginate(ITEMS, { limit: 2, order: 'asc', after: 'b' }, (item) => item.id);

    expect(page.object).toBe('list');
    expect(page.data.map((item) => item.id)).toEqual(['c', 'd']);
    expect(page.first_id).toBe('c');
    expect(page.last_id).toBe('d');
    expect(page.has_more).toBe(true);
  });

  test('reports the final page without more rows', () => {
    const page = paginate(ITEMS, { limit: 2, order: 'asc', after: 'd' }, (item) => item.id);

    expect(page.data.map((item) => item.id)).toEqual(['e']);
    expect(page.first_id).toBe('e');
    expect(page.last_id).toBe('e');
    expect(page.has_more).toBe(false);
  });

  test('returns an empty page with null IDs when the cursor is past the end', () => {
    const page = paginate(ITEMS, { limit: 2, order: 'asc', after: 'zzz' }, (item) => item.id);

    expect(page.data).toEqual([]);
    expect(page.first_id).toBeNull();
    expect(page.last_id).toBeNull();
    expect(page.has_more).toBe(false);
  });

  test('returns an empty page with null IDs for an empty collection', () => {
    const page = paginate([], { limit: 20, order: 'desc', after: null }, (item: { id: string }) => item.id);

    expect(page).toEqual({ object: 'list', data: [], first_id: null, last_id: null, has_more: false });
  });
});
