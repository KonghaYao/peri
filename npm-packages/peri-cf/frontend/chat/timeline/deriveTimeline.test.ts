import { describe, expect, test } from 'bun:test';
import type { Entry, ChatDisplayState, ToolCall, Turn } from '../runtime';

type EntryBlock = Entry['blocks'][number];
import { deriveTimeline, formatWorkDuration, turnFoldLabel } from './deriveTimeline';

const chat = { id: 'chat-timeline', title: '时间线测试', updatedAt: '2026-10-20T00:00:00Z' };

function textBlock(text: string, blockId = 'text:0'): EntryBlock {
  return { blockId, type: 'text', text };
}

function reasoningBlock(text: string, blockId: string, visibility: 'summary' | 'hidden' = 'summary'): EntryBlock {
  return { blockId, type: 'reasoning', text, visibility };
}

function toolCall(toolCallId: string, overrides: Partial<ToolCall> = {}): ToolCall {
  return {
    toolCallId, turnId: 'turn:1', name: 'Read', status: 'completed',
    createdAt: '2026-10-20T00:00:00Z', completedAt: '2026-10-20T00:00:01Z', error: null,
    arguments: { file_path: '/workspace/README.md' }, result: { contentText: 'ok' },
    ...overrides,
  };
}

function toolBlock(toolCallId: string, overrides: Partial<ToolCall> = {}, blockId = `tool:${toolCallId}`): EntryBlock {
  return { blockId, type: 'tool_call', toolCallId, tool: toolCall(toolCallId, overrides) };
}

function entry(overrides: Partial<Entry> = {}): Entry {
  return {
    entryId: 'turn:1:assistant', id: 'turn:1:assistant', kind: 'message', turnId: 'turn:1',
    role: 'assistant', status: 'completed', createdAt: '2026-10-20T00:00:00Z',
    createdAtSource: 'observed', completedAt: '2026-10-20T00:00:01Z', error: null,
    blocks: [textBlock('回答')],
    ...overrides,
  };
}

function user(turnId: string, entryId = `user:${turnId}`): Entry {
  return entry({
    entryId, id: entryId, turnId, role: 'user', status: 'completed',
    blocks: [textBlock(`问题 ${turnId}`)],
  });
}

function turn(turnId: string, overrides: Partial<Turn> = {}): Turn {
  return {
    turnId, status: 'streaming', startedAt: '2026-10-20T00:00:00Z', completedAt: null,
    error: null, interactionStatus: 'idle', ...overrides,
  };
}

function state(entries: Entry[], overrides: Partial<ChatDisplayState> = {}): ChatDisplayState {
  return {
    chat, sessionId: chat.id, bindingRevision: 1, running: false, executionBlocked: false, readiness: 'ready',
    entries, toolCalls: [], turns: [], activeTurn: { turnId: null, status: null }, session: {},
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [], pendingInteractions: [], inputQueue: null,
    ...overrides,
  };
}

describe('deriveTimeline', () => {
  test('orders user, work, assistant and merges consecutive reasoning/tool across entries', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a1', id: 'turn:1:a1', turnId: 'turn:1',
        blocks: [
          reasoningBlock('先看文件', 'r1'),
          toolBlock('t1'),
          textBlock('中间回答', 'text:1'),
          reasoningBlock('再看一遍', 'r2'),
          toolBlock('t2'),
        ],
      }),
      entry({
        entryId: 'turn:1:a2', id: 'turn:1:a2', turnId: 'turn:1',
        blocks: [toolBlock('t3')],
      }),
    ]));

    expect(rows.map((row) => row.kind)).toEqual(['user', 'work', 'assistant', 'work']);
    const work = rows[1];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    expect(work.items.map((item) => item.kind)).toEqual(['reasoning', 'tool']);
    expect(work.items[0]?.text).toBe('先看文件');
    expect(work.items[1]?.target).toBe('/workspace/README.md');

    const secondWork = rows[3];
    expect(secondWork?.kind).toBe('work');
    if (secondWork?.kind !== 'work') return;
    expect(secondWork.items.map((item) => item.kind)).toEqual(['reasoning', 'tool', 'tool']);
    expect(secondWork.items.map((item) => item.id)).toEqual(['turn:1:a1:r2', 't2', 't3']);
  });

  test('hidden reasoning is omitted without breaking a work group', () => {
    const rows = deriveTimeline(state([entry({
      blocks: [
        reasoningBlock('可见', 'r1'),
        reasoningBlock('不可见', 'r2', 'hidden'),
        toolBlock('t1'),
      ],
    })]));
    expect(rows).toHaveLength(1);
    const work = rows[0];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    expect(work.items.map((item) => item.id)).toEqual(['turn:1:assistant:r1', 't1']);
  });

  test('a missing tool record stays visible as a failed, unavailable item', () => {
    const rows = deriveTimeline(state([entry({
      blocks: [{
        blockId: 'tool:missing', type: 'tool_call', toolCallId: 'missing',
        tool: null, unavailable: 'object_unavailable',
      }],
    })]));
    const work = rows[0];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    expect(work.items[0]).toMatchObject({
      kind: 'tool', id: 'turn:1:assistant:tool:missing', toolCallId: 'missing',
      status: 'unavailable', failed: true,
      label: '工具调用不可用', unavailable: 'object_unavailable',
    });
  });

  test('cancelled tools are neutral while failed/error tools remain failures', () => {
    const rows = deriveTimeline(state([entry({
      blocks: [
        toolBlock('cancelled-tool', { status: 'cancelled', error: null }),
        toolBlock('failed-tool', { status: 'failed', error: null }),
        toolBlock('error-tool', { status: 'error', error: { code: 'tool_error', message: '失败' } }),
      ],
    })]));
    const work = rows[0];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    expect(work.items.map((item) => [item.status, item.failed])).toEqual([
      ['cancelled', false],
      ['failed', true],
      ['error', true],
    ]);
  });

  test('unknown blocks are preserved and split the work group', () => {
    const rows = deriveTimeline(state([entry({
      blocks: [
        toolBlock('t1'),
        { blockId: 'unknown:1', type: 'unknown', reason: 'unsupported_content' },
        toolBlock('t2'),
      ],
    })]));
    expect(rows.map((row) => row.kind)).toEqual(['work', 'unknown', 'work']);
    const unknown = rows[1];
    expect(unknown?.kind).toBe('unknown');
    if (unknown?.kind !== 'unknown') return;
    expect(unknown.reason).toBe('unsupported_content');
  });

  test('history turns are split by user messages even when assistant tool calls are adjacent', () => {
    const rows = deriveTimeline(state([
      user('history:1'),
      entry({ entryId: 'history:1:a', id: 'history:1:a', turnId: 'history:1', blocks: [toolBlock('h1')] }),
      entry({ entryId: 'history:1:b', id: 'history:1:b', turnId: 'history:1', blocks: [toolBlock('h2')] }),
      user('history:2'),
      entry({ entryId: 'history:2:a', id: 'history:2:a', turnId: 'history:2', blocks: [toolBlock('h3')] }),
    ]));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'work', 'user', 'work']);
    const firstWork = rows[1];
    expect(firstWork?.kind).toBe('work');
    if (firstWork?.kind !== 'work') return;
    expect(firstWork.items.map((item) => item.id)).toEqual(['h1', 'h2']);
  });

  test('running state appends a working row with the active turn start time', () => {
    const rows = deriveTimeline(state([
      user('turn:live'),
      entry({ entryId: 'turn:live:a', id: 'turn:live:a', turnId: 'turn:live', blocks: [textBlock('正在输出')] }),
    ], {
      running: true,
      activeTurn: { turnId: 'turn:live', status: 'streaming' },
      turns: [turn('turn:live', { startedAt: '2026-10-20T00:00:12Z' })],
    }));
    const last = rows.at(-1);
    expect(last).toMatchObject({ kind: 'working', turnId: 'turn:live', startedAt: '2026-10-20T00:00:12Z' });
  });

  test('entry errors become visible error rows', () => {
    const rows = deriveTimeline(state([entry({
      status: 'error',
      error: { code: 'tool_error', message: '工具执行失败' },
    })]));
    expect(rows.map((row) => row.kind)).toEqual(['assistant', 'error']);
    const errorRow = rows[1];
    expect(errorRow?.kind).toBe('error');
    if (errorRow?.kind !== 'error') return;
    expect(errorRow.message).toBe('工具执行失败');
  });

  test('not-ready projections do not fabricate rows', () => {
    expect(deriveTimeline(null)).toEqual([]);
    expect(deriveTimeline(state([entry()], { readiness: 'not-ready' }))).toEqual([]);
  });
});

describe('turn fold', () => {
  const finished = turn('turn:1', {
    status: 'completed', startedAt: '2026-10-20T00:00:00Z', completedAt: '2026-10-20T00:00:02Z',
  });
  const withWork = state([
    user('turn:1'),
    entry({
      entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
      blocks: [toolBlock('t1'), textBlock('结论', 'text:1')],
    }),
  ], { turns: [finished] });

  test('a settled turn collapses its work into Worked for X while keeping text', () => {
    const rows = deriveTimeline(withWork);
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'assistant']);
    const fold = rows[1];
    expect(fold?.kind).toBe('turn-fold');
    if (fold?.kind !== 'turn-fold') return;
    expect(fold).toMatchObject({
      turnId: 'turn:1', label: 'Worked for 2.0s', expanded: false,
      startedAt: '2026-10-20T00:00:00Z', completedAt: '2026-10-20T00:00:02Z',
    });
    expect(fold.summary).toBe('1 个工具');
  });

  test('an expanded turn fold keeps the original work rows in place', () => {
    const rows = deriveTimeline(withWork, { expandedTurnIds: new Set(['turn:1']) });
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'work', 'assistant']);
    const fold = rows[1];
    expect(fold?.kind).toBe('turn-fold');
    if (fold?.kind !== 'turn-fold') return;
    expect(fold.expanded).toBe(true);
    expect(fold.summary).toBe('1 个工具');
    const work = rows[2];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    expect(work.turnId).toBe('turn:1');
    expect(work.bare).toBe(true);
  });

  test('one fold per turn covers multiple non-contiguous work groups', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [toolBlock('t1'), textBlock('中间回答', 'text:1'), toolBlock('t2')],
      }),
    ], { turns: [finished] }));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'assistant']);
  });

  test('running turns never fold and keep their work rows active', () => {
    const live = state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', status: 'streaming',
        blocks: [toolBlock('t1', { status: 'running', completedAt: null })],
      }),
    ], {
      running: true,
      activeTurn: { turnId: 'turn:1', status: 'streaming' },
      turns: [turn('turn:1', { startedAt: '2026-10-20T00:00:00Z' })],
    });
    const rows = deriveTimeline(live);
    expect(rows.map((row) => row.kind)).toEqual(['user', 'work', 'working']);
    const work = rows[1];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    expect(work.active).toBe(true);
  });

  test('a completed turn folds even when it is still the activeTurn id', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [toolBlock('t1'), textBlock('结论', 'text:1')],
      }),
    ], {
      running: false,
      // TurnMachine.exit 只把状态改为 completed，不清 activeTurnId。
      activeTurn: { turnId: 'turn:1', status: 'completed' },
      turns: [finished],
    }));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'assistant']);
  });

  test('a live turn with running=true never folds even if its record says completed', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({ entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', blocks: [toolBlock('t1')] }),
    ], {
      running: true,
      activeTurn: { turnId: 'turn:1', status: 'completed' },
      turns: [finished],
    }));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'work', 'working']);
  });

  test('failed tools inside a completed turn are never hidden by the fold', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [toolBlock('failed', { status: 'failed' })],
      }),
    ], { turns: [finished] }));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'work']);
    const work = rows[2];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    expect(work.items[0]?.failed).toBe(true);
    expect(work.items[0]?.target).toBe('/workspace/README.md');
  });

  test('a failure in a later work group colors the whole-turn fold', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [
          toolBlock('t1'),
          textBlock('中间回答', 'text:1'),
          toolBlock('t2', { status: 'failed' }),
        ],
      }),
    ], { turns: [finished] }));
    const fold = rows.find((row) => row.kind === 'turn-fold');
    expect(fold).toMatchObject({ kind: 'turn-fold', failed: true });
    expect(fold?.kind === 'turn-fold' ? fold.label : '').toMatch(/^Failed after/);
    // 失败在第二组，第一组也不能被 fold 隐藏。
    expect(rows.filter((row) => row.kind === 'work')).toHaveLength(2);
  });

  test('a cancellation in a later work group labels the whole-turn fold as cancelled', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [
          toolBlock('t1'),
          textBlock('中间回答', 'text:1'),
          toolBlock('t2', { status: 'cancelled' }),
        ],
      }),
    ], { turns: [finished] }));
    const fold = rows.find((row) => row.kind === 'turn-fold');
    expect(fold).toMatchObject({ kind: 'turn-fold', failed: false });
    expect(fold?.kind === 'turn-fold' ? fold.label : '').toMatch(/^Cancelled after/);
  });

  test('failed or cancelled turns stay expanded with a status summary', () => {
    for (const status of ['error', 'cancelled']) {
      const rows = deriveTimeline(state([
        user('turn:1'),
        entry({
          entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', status,
          blocks: [toolBlock('t1', { status })],
        }),
      ], { turns: [turn('turn:1', { status, completedAt: '2026-10-20T00:00:02Z' })] }));
      expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'work']);
      const fold = rows[1];
      expect(fold).toMatchObject({ kind: 'turn-fold', status, expanded: true });
      expect(fold?.kind === 'turn-fold' ? fold.label : '').toContain(status === 'error' ? 'Failed' : 'Cancelled');
    }
  });

  test('failed or cancelled work can still be explicitly collapsed by the user', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({ entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', blocks: [toolBlock('t1')] }),
    ], { turns: [turn('turn:1', { status: 'cancelled', completedAt: '2026-10-20T00:00:02Z' })] }), {
      collapsedTurnIds: new Set(['turn:1']),
    });
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold']);
    const fold = rows[1];
    expect(fold?.kind === 'turn-fold' ? fold.expanded : true).toBe(false);
  });

  test('missing turn timestamps yield a duration-less label instead of a guess', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({ entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', blocks: [toolBlock('t1')] }),
    ], { turns: [turn('turn:1', { status: 'completed', completedAt: null })] }));
    const fold = rows[1];
    expect(fold?.kind).toBe('turn-fold');
    if (fold?.kind !== 'turn-fold') return;
    expect(fold.label).toBe('Worked');
    expect(turnFoldLabel({ startedAt: null, completedAt: '2026-10-20T00:00:02Z' })).toBe('Worked');
  });

  test('duration formatting stays compact across units and zero stays zero', () => {
    expect(formatWorkDuration(0)).toBe('0ms');
    expect(formatWorkDuration(0.4)).toBe('0ms');
    expect(formatWorkDuration(250)).toBe('250ms');
    expect(formatWorkDuration(2_000)).toBe('2.0s');
    expect(formatWorkDuration(12_400)).toBe('12s');
    expect(formatWorkDuration(65_000)).toBe('1m 5s');
  });

  test('payload_too_large is a degraded completed result, never a failure', () => {
    const degraded = toolBlock('large', {
      status: 'completed',
      error: { code: 'payload_too_large', message: '工具载荷超出会话接纳预算' },
    });
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({ entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', blocks: [degraded] }),
    ], { turns: [finished] }), { expandedTurnIds: new Set(['turn:1']) });

    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'work']);
    const fold = rows[1];
    expect(fold).toMatchObject({ kind: 'turn-fold', failed: false, expanded: true });
    expect(fold?.kind === 'turn-fold' ? fold.label : '').toBe('Worked for 2.0s');
    const work = rows[2];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    expect(work.failed).toBe(false);
    expect(work.items[0]).toMatchObject({ status: 'completed', failed: false, degraded: true });
  });

  test('failed tools in a completed turn keep a static fold that never hides them', () => {
    const withFailure = state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [toolBlock('failed', { status: 'error', error: { code: 'tool_failed', message: '工具执行失败' } })],
      }),
    ], { turns: [finished] });

    const rows = deriveTimeline(withFailure);
    const fold = rows.find((row) => row.kind === 'turn-fold');
    expect(fold).toMatchObject({ kind: 'turn-fold', failed: true, expanded: true, static: true });
    expect(rows.some((row) => row.kind === 'work')).toBe(true);

    // 用户显式折叠也不会让失败证据消失：折叠开关静态化，内容保持可见。
    const collapsed = deriveTimeline(withFailure, { collapsedTurnIds: new Set(['turn:1']) });
    expect(collapsed.find((row) => row.kind === 'turn-fold')).toMatchObject({
      kind: 'turn-fold', expanded: true, static: true,
    });
    expect(collapsed.some((row) => row.kind === 'work')).toBe(true);
  });

  test('the fold opens the turn and stays before the next user message', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [toolBlock('t1'), textBlock('第一轮回答', 'text:1')],
      }),
      user('turn:2'),
      entry({ entryId: 'turn:2:a', id: 'turn:2:a', turnId: 'turn:2', blocks: [textBlock('第二轮回答')] }),
    ], { turns: [finished] }));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'assistant', 'user', 'assistant']);
    expect(rows[1]).toMatchObject({ kind: 'turn-fold', id: 'turn-fold:turn:1' });
  });

  test('interleaved text and tools emit one fold at the first work group', () => {
    const base = state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [
          textBlock('先说明', 'text:1'),
          toolBlock('t1'),
          textBlock('再说明', 'text:2'),
          toolBlock('t2'),
        ],
      }),
    ], { turns: [finished] });
    expect(deriveTimeline(base).map((row) => row.kind))
      .toEqual(['user', 'assistant', 'turn-fold', 'assistant']);
    expect(deriveTimeline(base, { expandedTurnIds: new Set(['turn:1']) }).map((row) => row.kind))
      .toEqual(['user', 'assistant', 'turn-fold', 'work', 'assistant', 'work']);
  });

  test('a trailing entry error stays visible after the fold', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', status: 'error',
        blocks: [toolBlock('t1')], error: { code: 'tool_error', message: '工具执行失败' },
      }),
    ], { turns: [finished] }));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'error']);
  });

  test('an unknown block stays in place after the fold', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({
        entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1',
        blocks: [
          toolBlock('t1'),
          { blockId: 'unknown:1', type: 'unknown', reason: 'unsupported_content' },
          toolBlock('t2'),
        ],
      }),
    ], { turns: [finished] }), { expandedTurnIds: new Set(['turn:1']) });
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'work', 'unknown', 'work']);
  });

  test('turns without a settled record keep their work rows and emit no fold', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({ entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', blocks: [toolBlock('t1')] }),
    ]));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'work']);
    expect(rows.some((row) => row.kind === 'turn-fold')).toBe(false);
  });

  test('a settled turn folds before the next live turn work rows', () => {
    const rows = deriveTimeline(state([
      user('turn:1'),
      entry({ entryId: 'turn:1:a', id: 'turn:1:a', turnId: 'turn:1', blocks: [toolBlock('t1')] }),
      user('turn:2'),
      entry({
        entryId: 'turn:2:a', id: 'turn:2:a', turnId: 'turn:2', status: 'streaming',
        blocks: [toolBlock('t2', { status: 'running', completedAt: null })],
      }),
    ], {
      running: true,
      activeTurn: { turnId: 'turn:2', status: 'streaming' },
      turns: [finished, turn('turn:2', { status: 'streaming' })],
    }));
    expect(rows.map((row) => row.kind)).toEqual(['user', 'turn-fold', 'user', 'work', 'working']);
  });
});
