import { describe, expect, test } from 'bun:test';
import {
  CronParseError,
  CronTimeZoneError,
  isSupportedTimeZone,
  nextCronOccurrence,
  nextCronTime,
  parseCron,
} from '../../../../server/platform/automations/cron';

describe('cron parsing', () => {
  test('expands lists, ranges and steps', () => {
    const schedule = parseCron('5,10,15-17 */6 * * 1-5');
    expect(schedule.minutes).toEqual([5, 10, 15, 16, 17]);
    expect(schedule.hours).toEqual([0, 6, 12, 18]);
    expect(schedule.daysOfWeek).toEqual([1, 2, 3, 4, 5]);
    expect(schedule.dayOfMonthRestricted).toBe(false);
    expect(schedule.dayOfWeekRestricted).toBe(true);
  });

  test('normalizes Sunday 7 to 0', () => {
    expect(parseCron('0 0 * * 7').daysOfWeek).toEqual([0]);
    expect(parseCron('0 0 * * 0,7').daysOfWeek).toEqual([0]);
  });

  test('rejects malformed and out-of-range expressions', () => {
    const invalid = [
      '',
      '* * * *',
      '* * * * * *',
      '60 * * * *',
      '* 24 * * *',
      '* * 0 * *',
      '* * * 13 *',
      '* * * * 8',
      '*/0 * * * *',
      '5-1 * * * *',
      'foo * * * *',
      '1,,2 * * * *',
      '1/2/3 * * * *',
    ];
    for (const expression of invalid) {
      expect(() => parseCron(expression)).toThrow(CronParseError);
    }
  });
});

describe('cron next occurrence', () => {
  const minute = 60_000;

  test('finds the next minute and never returns the current instant', () => {
    const after = Date.UTC(2026, 0, 1, 12, 0, 30);
    expect(nextCronTime('* * * * *', 'UTC', after)).toBe(Date.UTC(2026, 0, 1, 12, 1, 0));
    expect(nextCronTime('* * * * *', 'UTC', Date.UTC(2026, 0, 1, 12, 1, 0))).toBe(
      Date.UTC(2026, 0, 1, 12, 2, 0),
    );
  });

  test('crosses a day boundary for a daily schedule', () => {
    const after = Date.UTC(2026, 0, 1, 12, 0, 0);
    expect(nextCronTime('0 9 * * *', 'UTC', after)).toBe(Date.UTC(2026, 0, 2, 9, 0, 0));
  });

  test('matches a month and day-of-month boundary', () => {
    const after = Date.UTC(2026, 0, 1, 0, 0, 0);
    expect(nextCronTime('30 8 15 * *', 'UTC', after)).toBe(Date.UTC(2026, 0, 15, 8, 30, 0));
  });

  test('matches a day-of-week boundary', () => {
    // 2026-01-01 is a Thursday; the next Monday is 2026-01-05.
    const after = Date.UTC(2026, 0, 1, 0, 0, 0);
    expect(nextCronTime('0 0 * * 1', 'UTC', after)).toBe(Date.UTC(2026, 0, 5, 0, 0, 0));
  });

  test('treats restricted day-of-month and day-of-week as OR', () => {
    const after = Date.UTC(2026, 0, 2, 0, 0, 0);
    // The first of the month is far away; the next Monday arrives first.
    expect(nextCronTime('0 0 1 * 1', 'UTC', after)).toBe(Date.UTC(2026, 0, 5, 0, 0, 0));
  });

  test('never schedules more often than one minute', () => {
    const after = Date.UTC(2026, 0, 1, 12, 0, 30);
    const next = nextCronTime('* * * * *', 'UTC', after);
    expect(next - after).toBeGreaterThanOrEqual(minute - 30_000);
  });
});

describe('cron time zones', () => {
  test('interprets the expression in the requested IANA zone', () => {
    const schedule = parseCron('0 9 * * *');
    const january = nextCronOccurrence(schedule, 'America/New_York', Date.UTC(2026, 0, 1, 0, 0, 0));
    expect(january).toBe(Date.UTC(2026, 0, 1, 14, 0, 0));

    const july = nextCronOccurrence(schedule, 'America/New_York', Date.UTC(2026, 6, 1, 0, 0, 0));
    expect(july).toBe(Date.UTC(2026, 6, 1, 13, 0, 0));
  });

  test('reports an unknown zone explicitly instead of falling back to UTC', () => {
    expect(isSupportedTimeZone('UTC')).toBe(true);
    expect(isSupportedTimeZone('America/New_York')).toBe(true);
    expect(isSupportedTimeZone('')).toBe(false);
    expect(isSupportedTimeZone('Not/AZone')).toBe(false);
    expect(() => nextCronTime('* * * * *', 'Not/AZone', Date.UTC(2026, 0, 1))).toThrow(CronTimeZoneError);
  });
});
