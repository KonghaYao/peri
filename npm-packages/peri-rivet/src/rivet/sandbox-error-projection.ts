/**
 * Bounded, credential-free projection for diagnostics.
 *
 * Diagnostics leave the credential boundary, so known secrets must be projected before any
 * observer (often a logger) sees them. Action results already collapse unknown failures; this is
 * the remaining sink that previously received raw runtime errors.
 */

const REDACTED = '[redacted]';
const MAX_PROJECTED_KEYS = 32;
const MAX_PROJECTED_DEPTH = 4;

function redactText(value: string, secret: string): string {
  return value.split(secret).join(REDACTED);
}

export function projectError(value: unknown, secret: string, seen = new Set<object>(), depth = 0): unknown {
  if (depth > MAX_PROJECTED_DEPTH) {
    return '[truncated]';
  }
  if (typeof value === 'string') {
    return redactText(value, secret);
  }
  if (typeof value === 'bigint') {
    return value.toString();
  }
  if (value === null || typeof value !== 'object') {
    return value;
  }
  if (seen.has(value)) {
    return '[circular]';
  }
  seen.add(value);
  if (value instanceof Error) {
    const projected: Record<string, unknown> = {
      name: redactText(value.name, secret),
      message: redactText(value.message, secret),
    };
    if (typeof value.stack === 'string') {
      projected.stack = redactText(value.stack, secret);
    }
    const code = (value as { readonly code?: unknown }).code;
    if (typeof code === 'string') {
      projected.code = redactText(code, secret);
    } else if (typeof code === 'number') {
      projected.code = code;
    }
    if (value.cause !== undefined) {
      projected.cause = projectError(value.cause, secret, seen, depth + 1);
    }
    return projected;
  }
  if (Array.isArray(value)) {
    return value.slice(0, MAX_PROJECTED_KEYS).map((item) => projectError(item, secret, seen, depth + 1));
  }
  const projected: Record<string, unknown> = {};
  let entries: Array<[string, unknown]>;
  try {
    entries = Object.entries(value);
  } catch {
    return '[unreadable]';
  }
  for (const [key, item] of entries.slice(0, MAX_PROJECTED_KEYS)) {
    projected[redactText(key, secret)] = projectError(item, secret, seen, depth + 1);
  }
  return projected;
}
