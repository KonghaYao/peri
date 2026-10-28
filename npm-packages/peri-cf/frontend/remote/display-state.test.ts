import { describe, expect, test } from 'bun:test';
import type { SessionView } from '@peri-code/sdk/view';
import { materializeChatDisplayState } from './display-state';

const chat = { id: 'session-1', title: '投影测试', updatedAt: '2026-10-20T00:00:00Z' };

function view(overrides: Record<string, unknown> = {}): SessionView {
  return {
    readiness: 'ready',
    schemaVersions: { chat: 3, session: 2 },
    sessionId: 'session-1',
    bindingRevision: 1,
    entries: [],
    toolCalls: [],
    turns: [],
    activeTurn: { turnId: null, status: null },
    activeTurnId: null,
    activeTurnStatus: null,
    session: {},
    tasks: [],
    plan: [],
    plansByTurn: {},
    interactions: [],
    pendingInteractions: [],
    inputQueue: null,
    running: false,
    executionBlocked: false,
    closed: false,
    closedReason: null,
    ...overrides,
  } as SessionView;
}

const reasoningEntry = {
  entryId: 'turn:1:assistant', id: 'turn:1:assistant', kind: 'message', turnId: 'turn:1',
  role: 'assistant', status: 'completed', createdAt: null, createdAtSource: 'unknown',
  completedAt: null, error: null,
  blocks: [{ blockId: 'r1', type: 'reasoning', text: '思考', visibility: 'opaque' }],
};

describe('Chat display state schema tolerance', () => {
  test('an SDK reasoning visibility outside the old enum no longer rejects the whole state', () => {
    const state = materializeChatDisplayState(view({ entries: [reasoningEntry] }), chat);
    expect(state.readiness).toBe('ready');
    expect(state.entries).toHaveLength(1);
    expect(state.entries[0]?.blocks[0]).toMatchObject({ type: 'reasoning', visibility: 'opaque' });
  });

  test('a malformed field falls back to valid facts instead of throwing', () => {
    const broken = view({
      entries: [reasoningEntry, { entryId: '', blocks: 'not-a-list' }],
      session: { title: 42 },
      inputQueue: { generation: 7, revision: 'x', items: 'nope' },
      running: true,
    });
    let state: ReturnType<typeof materializeChatDisplayState> | undefined;
    expect(() => { state = materializeChatDisplayState(broken, chat); }).not.toThrow();
    expect(state).toMatchObject({
      readiness: 'ready', sessionId: 'session-1', bindingRevision: 1, running: true,
      session: {}, inputQueue: null,
    });
    // 坏条目被丢弃，同批仍能校验的条目继续展示。
    expect(state?.entries).toHaveLength(1);
  });

  test('a not-ready replica still hides entry content without a parse error', () => {
    const state = materializeChatDisplayState(
      view({ readiness: 'not-ready', entries: [reasoningEntry], running: true }),
      chat,
    );
    expect(state).toMatchObject({ readiness: 'not-ready', entries: [], running: false });
  });
});
