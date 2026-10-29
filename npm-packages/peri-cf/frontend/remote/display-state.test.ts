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

const permission = {
  id: 'interaction-1', kind: 'permission', sessionId: 'session-1', status: 'pending',
  createdAt: null, resolvedAt: null, title: '允许执行？',
};

/** 覆盖全部展示字段的一组有效事实。 */
const facts = {
  running: true,
  executionBlocked: true,
  entries: [reasoningEntry],
  toolCalls: [{
    toolCallId: 'tool-1', turnId: 'turn:1', name: 'read_file', status: 'completed',
    createdAt: null, completedAt: null, error: null,
  }],
  turns: [{
    turnId: 'turn:1', status: 'completed', startedAt: null, completedAt: null,
    error: null, interactionStatus: 'none',
  }],
  activeTurn: { turnId: 'turn:1', status: 'completed' },
  session: { title: '会话标题', model: 'claude' },
  tasks: [{ taskId: 'task-1', kind: 'subagent', title: '子任务', status: 'running' }],
  plan: [{ content: '做一件事', status: 'pending' }],
  plansByTurn: { 'turn:1': [{ content: '做一件事', status: 'pending' }] },
  interactions: [permission],
  pendingInteractions: [permission],
  inputQueue: { generation: 'gen-1', revision: 1, items: [{ inputId: 'input-1', state: 'queued' }] },
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

describe('field mapping is shared by the normal and salvage paths', () => {
  test('a field-level fallback yields the same facts as a normalized view', () => {
    // 可展示子集：只保留一条有效条目 + 空 session；坏条目与坏 session 都应落到这里。
    const normalized = materializeChatDisplayState(
      view({ ...facts, entries: [reasoningEntry], session: {} }),
      chat,
    );
    const recovered = materializeChatDisplayState(
      view({
        ...facts,
        entries: [reasoningEntry, { entryId: '', blocks: 'not-a-list' }],
        session: { title: 42 },
      }),
      chat,
    );
    expect(recovered).toEqual(normalized);
  });

  test('the salvage path returns the same field values as the schema path', () => {
    const normalized = materializeChatDisplayState(view(facts), chat);
    // `chat.id` 为空触发整份 schema 拒绝，强制走 salvage；其余输入与上面相同。
    const salvaged = materializeChatDisplayState(view(facts), { ...chat, id: '' });
    expect({ ...salvaged, chat: normalized.chat }).toEqual(normalized);
  });
});
