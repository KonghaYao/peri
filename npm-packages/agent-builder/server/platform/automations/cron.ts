/**
 * A standard five-field cron parser and next-occurrence calculator.
 *
 * Fields are `minute hour day-of-month month day-of-week`. Day-of-week accepts `0` or `7` for
 * Sunday. As in Vixie cron, when both day fields are restricted an occurrence matches either one.
 * Time zones are IANA names interpreted through `Intl.DateTimeFormat`; a zone the runtime does not
 * know is reported explicitly instead of silently falling back to UTC.
 */

const MAX_EXPRESSION_LENGTH = 256;
const MAX_SEARCH_DAYS = 366 * 8;
const formatterCache = new Map<string, Intl.DateTimeFormat>();

export class CronParseError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'CronParseError';
  }
}

export class CronTimeZoneError extends Error {
  readonly timeZone: string;

  constructor(timeZone: string) {
    super('Unsupported time zone');
    this.name = 'CronTimeZoneError';
    this.timeZone = timeZone;
  }
}

export interface CronSchedule {
  readonly minutes: readonly number[];
  readonly hours: readonly number[];
  readonly daysOfMonth: readonly number[];
  readonly months: readonly number[];
  readonly daysOfWeek: readonly number[];
  readonly dayOfMonthRestricted: boolean;
  readonly dayOfWeekRestricted: boolean;
}

interface LocalParts {
  readonly year: number;
  readonly month: number;
  readonly day: number;
  readonly hour: number;
  readonly minute: number;
  readonly second: number;
}

interface FieldOptions {
  readonly name: string;
  readonly min: number;
  readonly max: number;
  readonly normalize?: (value: number) => number;
}

function isIntegerText(value: string): boolean {
  return /^\d+$/.test(value);
}

function parseField(raw: string, options: FieldOptions): { values: number[]; restricted: boolean } {
  if (raw.length === 0) throw new CronParseError(`${options.name} is empty`);
  const values = new Set<number>();
  const restricted = !raw.startsWith('*');

  for (const part of raw.split(',')) {
    if (part.length === 0) throw new CronParseError(`${options.name} has an empty list item`);
    const segments = part.split('/');
    if (segments.length > 2) throw new CronParseError(`${options.name} has too many step separators`);
    const range = segments[0] ?? '';
    const stepText = segments[1];

    let step = 1;
    if (stepText !== undefined) {
      if (!isIntegerText(stepText)) throw new CronParseError(`${options.name} has an invalid step`);
      step = Number(stepText);
      if (!Number.isSafeInteger(step) || step < 1) {
        throw new CronParseError(`${options.name} has an invalid step`);
      }
    }

    let start: number;
    let end: number;
    if (range === '*') {
      start = options.min;
      end = options.max;
    } else if (range.includes('-')) {
      const bounds = range.split('-');
      if (bounds.length !== 2) throw new CronParseError(`${options.name} has an invalid range`);
      const [startText = '', endText = ''] = bounds;
      if (!isIntegerText(startText) || !isIntegerText(endText)) {
        throw new CronParseError(`${options.name} has an invalid range`);
      }
      start = Number(startText);
      end = Number(endText);
    } else {
      if (!isIntegerText(range)) throw new CronParseError(`${options.name} has an invalid value`);
      start = Number(range);
      end = stepText === undefined ? start : options.max;
    }

    if (start < options.min || start > options.max || end < options.min || end > options.max || start > end) {
      throw new CronParseError(`${options.name} is outside its allowed range`);
    }

    for (let value = start; value <= end; value += step) {
      values.add(options.normalize === undefined ? value : options.normalize(value));
    }
  }

  if (values.size === 0) throw new CronParseError(`${options.name} has no values`);
  return { values: [...values].sort((left, right) => left - right), restricted };
}

export function parseCron(expression: string): CronSchedule {
  if (expression.length === 0 || expression.length > MAX_EXPRESSION_LENGTH) {
    throw new CronParseError('Cron expression has an invalid length');
  }
  const fields = expression.trim().split(/\s+/);
  if (fields.length !== 5) throw new CronParseError('Cron expression must have five fields');
  const [minuteText = '', hourText = '', dayText = '', monthText = '', weekText = ''] = fields;
  const minute = parseField(minuteText, { name: 'minute', min: 0, max: 59 });
  const hour = parseField(hourText, { name: 'hour', min: 0, max: 23 });
  const day = parseField(dayText, { name: 'day-of-month', min: 1, max: 31 });
  const month = parseField(monthText, { name: 'month', min: 1, max: 12 });
  const week = parseField(weekText, {
    name: 'day-of-week',
    min: 0,
    max: 7,
    normalize: (value) => (value === 7 ? 0 : value),
  });
  return {
    minutes: minute.values,
    hours: hour.values,
    daysOfMonth: day.values,
    months: month.values,
    daysOfWeek: week.values,
    dayOfMonthRestricted: day.restricted,
    dayOfWeekRestricted: week.restricted,
  };
}

export function isSupportedTimeZone(timeZone: string): boolean {
  if (timeZone.length === 0) return false;
  try {
    formatterFor(timeZone).format(0);
    return true;
  } catch {
    formatterCache.delete(timeZone);
    return false;
  }
}

function formatterFor(timeZone: string): Intl.DateTimeFormat {
  const cached = formatterCache.get(timeZone);
  if (cached !== undefined) return cached;
  const formatter = new Intl.DateTimeFormat('en-US', {
    timeZone,
    hourCycle: 'h23',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
  formatterCache.set(timeZone, formatter);
  return formatter;
}

function localParts(timestamp: number, timeZone: string): LocalParts {
  const parts = formatterFor(timeZone).formatToParts(new Date(timestamp));
  let year = 0;
  let month = 0;
  let day = 0;
  let hour = 0;
  let minute = 0;
  let second = 0;
  for (const part of parts) {
    const value = Number(part.value);
    if (!Number.isFinite(value)) continue;
    switch (part.type) {
      case 'year':
        year = value;
        break;
      case 'month':
        month = value;
        break;
      case 'day':
        day = value;
        break;
      case 'hour':
        hour = value;
        break;
      case 'minute':
        minute = value;
        break;
      case 'second':
        second = value;
        break;
      default:
        break;
    }
  }
  return { year, month, day, hour, minute, second };
}

function zoneOffset(timestamp: number, timeZone: string): number {
  const parts = localParts(timestamp, timeZone);
  return Date.UTC(parts.year, parts.month - 1, parts.day, parts.hour, parts.minute, parts.second) - timestamp;
}

/** Converts a wall-clock time in `timeZone` to an instant; DST gaps are resolved on a best effort. */
function zonedTimeToUtc(
  year: number,
  month: number,
  day: number,
  hour: number,
  minute: number,
  timeZone: string,
): number {
  const guess = Date.UTC(year, month - 1, day, hour, minute, 0);
  const offset = zoneOffset(guess, timeZone);
  let timestamp = guess - offset;
  const refined = zoneOffset(timestamp, timeZone);
  if (refined !== offset) timestamp = guess - refined;
  return timestamp;
}

function dayMatches(schedule: CronSchedule, year: number, month: number, day: number): boolean {
  if (!schedule.months.includes(month)) return false;
  const matchesDayOfMonth = schedule.daysOfMonth.includes(day);
  const weekday = new Date(Date.UTC(year, month - 1, day)).getUTCDay();
  const matchesDayOfWeek = schedule.daysOfWeek.includes(weekday);
  if (schedule.dayOfMonthRestricted && schedule.dayOfWeekRestricted) {
    return matchesDayOfMonth || matchesDayOfWeek;
  }
  if (schedule.dayOfMonthRestricted) return matchesDayOfMonth;
  if (schedule.dayOfWeekRestricted) return matchesDayOfWeek;
  return true;
}

/**
 * Returns the first occurrence strictly after `afterMs` as a Unix timestamp in milliseconds.
 * A zone the runtime does not know raises `CronTimeZoneError`; an expression with no occurrence in
 * the eight-year search window raises `CronParseError`.
 */
export function nextCronOccurrence(schedule: CronSchedule, timeZone: string, afterMs: number): number {
  if (!isSupportedTimeZone(timeZone)) throw new CronTimeZoneError(timeZone);
  const local = localParts(afterMs, timeZone);
  const startDay = Date.UTC(local.year, local.month - 1, local.day);

  for (let offset = 0; offset <= MAX_SEARCH_DAYS; offset += 1) {
    const dayDate = new Date(startDay + offset * 86_400_000);
    const year = dayDate.getUTCFullYear();
    const month = dayDate.getUTCMonth() + 1;
    const day = dayDate.getUTCDate();
    if (!dayMatches(schedule, year, month, day)) continue;

    for (const hour of schedule.hours) {
      for (const minute of schedule.minutes) {
        const timestamp = zonedTimeToUtc(year, month, day, hour, minute, timeZone);
        if (timestamp <= afterMs) continue;
        // A wall time inside a DST gap resolves to a different local time; skip it and keep
        // searching instead of firing at the wrong hour.
        const resolved = localParts(timestamp, timeZone);
        if (
          resolved.year !== year ||
          resolved.month !== month ||
          resolved.day !== day ||
          resolved.hour !== hour ||
          resolved.minute !== minute
        ) {
          continue;
        }
        return timestamp;
      }
    }
  }

  throw new CronParseError('Cron expression has no occurrence in the search window');
}

/** Convenience wrapper that parses first; use `parseCron` once when computing repeatedly. */
export function nextCronTime(expression: string, timeZone: string, afterMs: number): number {
  return nextCronOccurrence(parseCron(expression), timeZone, afterMs);
}
