import * as Y from 'yjs';

export function asMap(value: unknown): Y.Map<unknown> | null {
  return value instanceof Y.Map ? value : null;
}

export function asArray(value: unknown): Y.Array<unknown> | null {
  return value instanceof Y.Array ? value : null;
}

export function getStr(map: Y.Map<unknown> | null, key: string): string | null {
  const value = map?.get(key);
  return value === undefined || value === null ? null : String(value);
}

export function getNum(map: Y.Map<unknown> | null, key: string): number | null {
  const value = map?.get(key);
  if (value === undefined || value === null) return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

export function yText(value: unknown): string | null {
  if (
    value
    && typeof (value as { toString?: unknown }).toString === 'function'
    && !(value instanceof Y.Map)
    && !(value instanceof Y.Array)
  ) {
    return (value as { toString(): string }).toString();
  }
  return null;
}

/** Assistant skeletons may temporarily contain an empty timestamp. */
export function safeTime(value: unknown): string {
  return value ? String(value) : '—';
}

/** Convert redacted server projections to inert presentation data. */
export function yValue(value: unknown): unknown {
  if (value instanceof Y.Map) {
    return Object.fromEntries([...value.entries()].map(([key, item]) => [key, yValue(item)]));
  }
  if (value instanceof Y.Array) return value.toArray().map(yValue);
  if (value instanceof Y.Text) return value.toString();
  return value;
}
