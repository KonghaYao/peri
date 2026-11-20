/** Helpers for JSON columns and the Unix-second timestamps used by the platform wire format. */

export const EMPTY_JSON_OBJECT = '{}';
export const EMPTY_JSON_ARRAY = '[]';

export function nowUnixSeconds(now: number = Date.now()): number {
  return Math.floor(now / 1000);
}

export function millisToUnixSeconds(millis: number): number {
  return Math.floor(millis / 1000);
}

export function unixSecondsToMillis(seconds: number): number {
  return seconds * 1000;
}

/** Parses a JSON column, falling back to a copy of `fallback` when the stored value is unusable. */
export function parseJsonColumn<T>(raw: string | null | undefined, fallback: T): T {
  if (raw === null || raw === undefined || raw.length === 0) return structuredClone(fallback);
  try {
    return JSON.parse(raw) as T;
  } catch {
    return structuredClone(fallback);
  }
}

export function serializeJsonColumn(value: unknown): string {
  return JSON.stringify(value);
}

/** Parses a string-to-string metadata column; non-string values are dropped. */
export function parseMetadataColumn(raw: string | null | undefined): Record<string, string> {
  const parsed = parseJsonColumn<unknown>(raw, {});
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return {};
  const metadata: Record<string, string> = {};
  for (const [key, value] of Object.entries(parsed)) {
    if (typeof value === 'string') metadata[key] = value;
  }
  return metadata;
}

export function serializeMetadataColumn(metadata: Readonly<Record<string, string>>): string {
  return serializeJsonColumn(metadata);
}

/** Adds the `object` discriminator expected by list and resource responses. */
export function withObject<T extends string, F extends Record<string, unknown>>(
  object: T,
  fields: F,
): F & { object: T } {
  return { ...fields, object };
}
