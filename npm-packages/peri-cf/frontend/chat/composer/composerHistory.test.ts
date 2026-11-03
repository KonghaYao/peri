import { describe, expect, test } from 'bun:test';
import type { ChatDisplayState } from '../runtime';
import { deriveHistoryPrompts, historyRecallDirection, stepHistory } from './composerHistory';

function entry(role: 'user' | 'assistant', text: string): ChatDisplayState['entries'][number] {
  return {
    entryId: `${role}:${text}`, id: `${role}:${text}`, kind: 'message', turnId: 't1', role,
    status: 'completed', createdAt: null, createdAtSource: 'observed', completedAt: null,
    error: null, blocks: [{ blockId: 'b1', type: 'text', text }],
  };
}

function state(entries: ChatDisplayState['entries']): ChatDisplayState {
  return {
    chat: { id: 'chat-1', title: 'Chat', updatedAt: '2026-10-20T00:00:00Z' },
    sessionId: 'chat-1', bindingRevision: 1, running: false, executionBlocked: false,
    readiness: 'ready', entries, toolCalls: [], turns: [],
    activeTurn: { turnId: null, status: null }, session: {},
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [],
    pendingInteractions: [], inputQueue: null,
  };
}

const key = (overrides: Partial<Parameters<typeof historyRecallDirection>[0]> = {}) => ({
  key: 'ArrowUp', shiftKey: false, altKey: false, ctrlKey: false, metaKey: false,
  isComposing: false, keyCode: 38, ...overrides,
});

describe('history prompts', () => {
  test('derives only non-empty user prompts in chronological order', () => {
    expect(deriveHistoryPrompts(state([
      entry('user', '第一条'),
      entry('assistant', '回答'),
      entry('user', '   '),
      entry('user', '第二条'),
    ]))).toEqual(['第一条', '第二条']);
    expect(deriveHistoryPrompts(null)).toEqual([]);
  });

  test('previous starts at the newest prompt and moves older', () => {
    expect(stepHistory(['a', 'b', 'c'], -1, 'previous')).toEqual({ index: 2, text: 'c' });
    expect(stepHistory(['a', 'b', 'c'], 2, 'previous')).toEqual({ index: 1, text: 'b' });
    expect(stepHistory(['a', 'b', 'c'], 0, 'previous')).toEqual({ index: 0, text: 'a' });
    expect(stepHistory([], -1, 'previous')).toBeNull();
  });

  test('next moves forward and clears past the newest prompt', () => {
    expect(stepHistory(['a', 'b', 'c'], -1, 'next')).toBeNull();
    expect(stepHistory(['a', 'b', 'c'], 0, 'next')).toEqual({ index: 1, text: 'b' });
    expect(stepHistory(['a', 'b', 'c'], 2, 'next')).toEqual({ index: -1, text: '' });
  });

  test('only bare arrows trigger recall; IME and modifiers do not', () => {
    expect(historyRecallDirection(key())).toBe('previous');
    expect(historyRecallDirection(key({ key: 'ArrowDown', keyCode: 40 }))).toBe('next');
    expect(historyRecallDirection(key({ shiftKey: true }))).toBeNull();
    expect(historyRecallDirection(key({ altKey: true }))).toBeNull();
    expect(historyRecallDirection(key({ ctrlKey: true }))).toBeNull();
    expect(historyRecallDirection(key({ metaKey: true }))).toBeNull();
    expect(historyRecallDirection(key({ isComposing: true }))).toBeNull();
    expect(historyRecallDirection(key({ keyCode: 229 }))).toBeNull();
    expect(historyRecallDirection(key({ key: 'ArrowLeft' }))).toBeNull();
  });
});
