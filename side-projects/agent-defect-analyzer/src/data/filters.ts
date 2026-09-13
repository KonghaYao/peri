/** Shared runtime validation for thread scope and UTC half-open date filters. */

export type ThreadScope = "roots" | "children" | "all";

export interface ValidatedThreadFilter {
  scope: ThreadScope;
  includeHidden: boolean;
  since: number | null;
  until: number | null;
  sinceText: string | null;
  untilText: string | null;
}

export function validateThreadScope(value: unknown): ThreadScope {
  if (value === undefined) return "roots";
  if (value === "roots" || value === "children" || value === "all") return value;
  throw new RangeError("scope must be roots, children, or all");
}

export function parseUtcIsoDate(value: string | undefined, field: string): number | null {
  if (value === undefined) return null;
  const isoWithTimezone = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/;
  if (!isoWithTimezone.test(value)) throw new RangeError(`${field} must be an ISO date-time with timezone`);
  const parts = value.match(/^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})/);
  if (!parts) throw new RangeError(`${field} must be an ISO date-time with timezone`);
  const year = Number(parts[1]);
  const month = Number(parts[2]);
  const day = Number(parts[3]);
  const hour = Number(parts[4]);
  const minute = Number(parts[5]);
  const second = Number(parts[6]);
  const daysInMonth = new Date(Date.UTC(year, month, 0)).getUTCDate();
  if (month < 1 || month > 12 || day < 1 || day > daysInMonth || hour > 23 || minute > 59 || second > 59) {
    throw new RangeError(`${field} must be an ISO date-time with timezone`);
  }
  const parsed = Date.parse(value);
  if (!Number.isFinite(parsed)) throw new RangeError(`${field} must be an ISO date-time with timezone`);
  return parsed;
}

export function validateThreadFilter(input: {
  scope?: unknown;
  includeHidden?: unknown;
  since?: string;
  until?: string;
}): ValidatedThreadFilter {
  const scope = validateThreadScope(input.scope);
  const includeHidden = input.includeHidden ?? false;
  if (typeof includeHidden !== "boolean") throw new RangeError("includeHidden must be boolean");
  const since = parseUtcIsoDate(input.since, "since");
  const until = parseUtcIsoDate(input.until, "until");
  if (since !== null && until !== null && since >= until) throw new RangeError("since must be earlier than until");
  return {
    scope,
    includeHidden,
    since,
    until,
    sinceText: since === null ? null : new Date(since).toISOString(),
    untilText: until === null ? null : new Date(until).toISOString(),
  };
}
