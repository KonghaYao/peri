import { describe, expect, test } from 'bun:test';
import {
  rememberTimelinePosition, restoreTimelinePosition, type TimelinePosition,
} from './timelinePositions';
import type { TimelineRow } from './types';

function workRow(id: string): TimelineRow {
  return {
    kind: 'work', id, turnId: 'turn:1', active: false,
    failed: false, defaultExpanded: false, forceExpanded: false,
    items: [{
      kind: 'tool', id: `${id}:t`, entryId: 'e', blockId: 'b', status: 'completed',
      label: 'Read', createdAt: null, completedAt: null, failed: false,
    }],
  };
}

const rows = [workRow('work:a'), workRow('work:b'), workRow('work:c')];

describe('timeline position restore model', () => {
  test('positions are isolated per chat and survive as in-memory facts', () => {
    const store = new Map<string, TimelinePosition>();
    store.set('chat-a', { atEnd: true, rowId: null, offsetWithinRow: 0, scrollOffset: 0 });
    store.set('chat-b', { atEnd: false, rowId: 'work:b', offsetWithinRow: 40, scrollOffset: 500 });
    expect(store.get('chat-a')?.atEnd).toBe(true);
    expect(store.get('chat-b')).toMatchObject({ atEnd: false, rowId: 'work:b', offsetWithinRow: 40 });
    expect(store.get('chat-c')).toBeUndefined();
    store.clear();
    expect(store.get('chat-a')).toBeUndefined();
  });

  test('pinned content is remembered as at-end and restores to the end', () => {
    const position = rememberTimelinePosition(
      { scrollHeight: 1_000, scrollTop: 880, clientHeight: 120 }, 'work:c', 900,
    );
    expect(position).toMatchObject({ atEnd: true, rowId: null, offsetWithinRow: 0 });
    expect(restoreTimelinePosition(position, rows)).toEqual({ initialScrollAtEnd: true });
  });

  test('scrolling up remembers the visible row and restores its index plus offset', () => {
    const position = rememberTimelinePosition(
      { scrollHeight: 1_000, scrollTop: 500, clientHeight: 120 }, 'work:b', 460,
    );
    expect(position).toMatchObject({ atEnd: false, rowId: 'work:b', offsetWithinRow: 40, scrollOffset: 500 });
    expect(restoreTimelinePosition(position, rows)).toEqual({
      initialScrollAtEnd: false,
      initialScrollIndex: { index: 1, viewPosition: 0, viewOffset: -40 },
    });
  });

  test('returning to the bottom overwrites the saved scroll-away position', () => {
    const store = new Map<string, TimelinePosition>();
    store.set('chat-a', { atEnd: false, rowId: 'work:b', offsetWithinRow: 40, scrollOffset: 500 });
    store.set('chat-a', rememberTimelinePosition(
      { scrollHeight: 1_200, scrollTop: 1_080, clientHeight: 120 }, 'work:c', 1_000,
    ));
    expect(restoreTimelinePosition(store.get('chat-a'), rows)).toEqual({ initialScrollAtEnd: true });
  });

  test('a removed anchor row falls back to the saved pixel offset', () => {
    expect(restoreTimelinePosition(
      { atEnd: false, rowId: 'work:missing', offsetWithinRow: 12, scrollOffset: 320 }, rows,
    )).toEqual({ initialScrollAtEnd: false, initialScrollOffset: 320 });
  });

  test('unknown chat starts at the end, preserving the previous bottom-follow behavior', () => {
    expect(restoreTimelinePosition(undefined, rows)).toEqual({ initialScrollAtEnd: true });
  });
});
