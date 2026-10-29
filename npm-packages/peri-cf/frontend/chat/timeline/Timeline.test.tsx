import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Entry, ChatDisplayState } from '../runtime';
import { deriveTimeline } from './deriveTimeline';
import { Timeline, TimelineRows } from './Timeline';
import { ToolRow } from './rows/ToolRow';

const chat = { id: 'chat-static', title: '静态渲染', updatedAt: '2026-10-20T00:00:00Z' };

const entries: Entry[] = [
  {
    entryId: 'turn:live:user', id: 'turn:live:user', kind: 'message', turnId: 'turn:live',
    role: 'user', status: 'completed', createdAt: '2026-10-20T00:00:00Z', createdAtSource: 'observed',
    completedAt: null, error: null, blocks: [{ blockId: 'text:0', type: 'text', text: '帮我看看 README' }],
  },
  {
    entryId: 'turn:live:a', id: 'turn:live:a', kind: 'message', turnId: 'turn:live',
    role: 'assistant', status: 'streaming', createdAt: '2026-10-20T00:00:01Z', createdAtSource: 'observed',
    completedAt: null, error: null,
    blocks: [
      { blockId: 'r1', type: 'reasoning', text: '先定位文件', visibility: 'summary' },
      {
        blockId: 'tool:read-1', type: 'tool_call', toolCallId: 'read-1',
        tool: {
          toolCallId: 'read-1', turnId: 'turn:live', name: 'Read', status: 'running',
          createdAt: '2026-10-20T00:00:02Z', completedAt: null, error: null,
          arguments: { file_path: '/workspace/README.md' },
        },
      },
      { blockId: 'unknown:1', type: 'unknown', reason: 'unsupported_content' },
      { blockId: 'text:1', type: 'text', text: '正在读取。' },
    ],
  },
  {
    entryId: 'turn:live:error', id: 'turn:live:error', kind: 'tool', turnId: 'turn:live',
    role: 'assistant', status: 'error', createdAt: '2026-10-20T00:00:03Z', createdAtSource: 'observed',
    completedAt: null, error: { code: 'tool_error', message: '工具执行失败' }, blocks: [],
  },
];

const state: ChatDisplayState = {
  chat, sessionId: chat.id, bindingRevision: 4, running: true, executionBlocked: false, readiness: 'ready',
  entries, toolCalls: [], turns: [{
    turnId: 'turn:live', status: 'streaming', startedAt: '2026-10-20T00:00:00Z', completedAt: null,
    error: null, interactionStatus: 'idle',
  }],
  activeTurn: { turnId: 'turn:live', status: 'streaming' }, session: {},
  tasks: [], plans: { current: [], byTurn: {} }, interactions: [], pendingInteractions: [], inputQueue: null,
};

describe('TimelineRows static rendering', () => {
  test('renders every supported row kind and keeps errors and unknown blocks visible', () => {
    const html = renderToStaticMarkup(<TimelineRows rows={deriveTimeline(state)} />);
    expect(html).toContain('data-timeline-row-kind="user"');
    expect(html).toContain('data-timeline-row-kind="work"');
    expect(html).toContain('data-timeline-row-kind="unknown"');
    expect(html).toContain('data-timeline-row-kind="assistant"');
    expect(html).toContain('data-timeline-row-kind="error"');
    expect(html).toContain('data-timeline-row-kind="working"');

    expect(html).toContain('帮我看看 README');
    expect(html).toContain('先定位文件');
    expect(html).toContain('读取文件');
    expect(html).toContain('/workspace/README.md');
    expect(html).toContain('暂不支持的内容块（unsupported_content）');
    expect(html).toContain('工具执行失败');
    expect(html).toContain('Working for');
  });

  test('cancelled tool rows stay neutral and do not use the failure style', () => {
    const cancelled: Entry = {
      entryId: 'turn:stop:a', id: 'turn:stop:a', kind: 'message', turnId: 'turn:stop',
      role: 'assistant', status: 'cancelled', createdAt: '2026-10-20T00:00:00Z',
      createdAtSource: 'observed', completedAt: '2026-10-20T00:00:01Z', error: null,
      blocks: [{
        blockId: 'tool:cancelled', type: 'tool_call', toolCallId: 'cancelled',
        tool: {
          toolCallId: 'cancelled', turnId: 'turn:stop', name: 'Bash', status: 'cancelled',
          createdAt: '2026-10-20T00:00:00Z', completedAt: '2026-10-20T00:00:01Z', error: null,
          arguments: { command: 'sleep 10' },
        },
      }],
    };
    const rows = deriveTimeline({
      ...state, entries: [cancelled], turns: [], activeTurn: { turnId: null, status: null }, running: false,
    });
    const work = rows[0];
    expect(work?.kind).toBe('work');
    if (work?.kind !== 'work') return;
    const html = renderToStaticMarkup(<ToolRow item={work.items[0]!} />);
    expect(html).toContain('已取消');
    expect(html).toContain('data-tool-failed="false"');
    expect(html).not.toContain('data-tool-failed="true"');
  });
});

describe('Timeline states', () => {
  const empty: ChatDisplayState = {
    ...state, entries: [], turns: [], activeTurn: { turnId: null, status: null }, running: false,
  };

  test('shows the injected empty state before any chat is selected', () => {
    const html = renderToStaticMarkup(
      <Timeline
        state={null}
        loadState="idle"
        emptyContent={<p>从这里开始</p>}
        bottomInset={0}
        scrollSignal={null}
      />,
    );
    expect(html).toContain('从这里开始');
    expect(html).not.toContain('peri-timeline-skeleton');
  });

  test('shows the skeleton while history has not arrived', () => {
    const html = renderToStaticMarkup(
      <Timeline state={null} loadState="loading" bottomInset={0} scrollSignal={null} />,
    );
    expect(html).toContain('peri-timeline-skeleton');
    expect(html).not.toContain('peri-timeline-error');
  });

  test('shows the injected empty state after history is ready', () => {
    const html = renderToStaticMarkup(
      <Timeline
        state={empty}
        loadState="ready"
        emptyContent={<p>从这里开始</p>}
        bottomInset={0}
        scrollSignal={null}
      />,
    );
    expect(html).toContain('从这里开始');
    expect(html).toContain('data-timeline-state="ready"');
    expect(html).not.toContain('peri-timeline-skeleton');
  });

  test('a sync failure replaces the permanent skeleton with an error state', () => {
    const html = renderToStaticMarkup(
      <Timeline
        state={null}
        loadState="failed"
        error="会话读取失败"
        emptyContent={<p>从这里开始</p>}
        bottomInset={0}
        scrollSignal={null}
      />,
    );
    expect(html).toContain('data-slot="peri-timeline-error"');
    expect(html).toContain('会话读取失败');
    expect(html).not.toContain('peri-timeline-skeleton');
    expect(html).not.toContain('从这里开始');
  });

  test('dismissing the error banner keeps the failed state instead of the skeleton', () => {
    const html = renderToStaticMarkup(
      <Timeline
        state={null}
        loadState="failed"
        emptyContent={<p>从这里开始</p>}
        bottomInset={0}
        scrollSignal={null}
      />,
    );
    expect(html).toContain('data-slot="peri-timeline-error"');
    expect(html).toContain('会话加载失败');
    expect(html).not.toContain('peri-timeline-skeleton');
  });

  test('a failed load with a retry action renders an active reload button', () => {
    const html = renderToStaticMarkup(
      <Timeline
        state={null}
        loadState="failed"
        onRetry={() => {}}
        bottomInset={0}
        scrollSignal={null}
      />,
    );
    expect(html).toContain('重新加载');
  });
});

describe('Timeline turn folds', () => {
  const finished: ChatDisplayState = {
    ...state,
    running: false,
    activeTurn: { turnId: null, status: null },
    turns: [{
      turnId: 'turn:done', status: 'completed', startedAt: '2026-10-20T00:00:00Z',
      completedAt: '2026-10-20T00:00:02Z', error: null, interactionStatus: 'idle',
    }],
    entries: [
      {
        entryId: 'turn:done:user', id: 'turn:done:user', kind: 'message', turnId: 'turn:done',
        role: 'user', status: 'completed', createdAt: '2026-10-20T00:00:00Z',
        createdAtSource: 'observed', completedAt: null, error: null,
        blocks: [{ blockId: 'text:0', type: 'text', text: '问题' }],
      },
      {
        entryId: 'turn:done:a', id: 'turn:done:a', kind: 'message', turnId: 'turn:done',
        role: 'assistant', status: 'completed', createdAt: '2026-10-20T00:00:01Z',
        createdAtSource: 'observed', completedAt: '2026-10-20T00:00:02Z', error: null,
        blocks: [
          {
            blockId: 'tool:read', type: 'tool_call', toolCallId: 'read',
            tool: {
              toolCallId: 'read', turnId: 'turn:done', name: 'Read', status: 'completed',
              createdAt: '2026-10-20T00:00:01Z', completedAt: '2026-10-20T00:00:02Z', error: null,
              arguments: { file_path: '/workspace/README.md' },
            },
          },
          { blockId: 'text:1', type: 'text', text: '最终回答' },
        ],
      },
    ],
  };

  test('collapsed turns render a Worked for X row and hide the work rows', () => {
    const html = renderToStaticMarkup(<TimelineRows rows={deriveTimeline(finished)} />);
    expect(html).toContain('data-timeline-row-kind="turn-fold"');
    expect(html).toContain('Worked for 2.0s');
    expect(html).toContain('最终回答');
    expect(html).not.toContain('data-timeline-row-kind="work"');
    // 折叠入口在首个 work 组之前，而不是回复末尾。
    expect(html.indexOf('data-timeline-row-kind="turn-fold"'))
      .toBeLessThan(html.indexOf('data-timeline-row-kind="assistant"'));
  });

  test('expanded turns render the underlying work rows', () => {
    const rows = deriveTimeline(finished, { expandedTurnIds: new Set(['turn:done']) });
    const html = renderToStaticMarkup(<TimelineRows rows={rows} onToggleTurnFold={() => {}} />);
    expect(html).toContain('data-timeline-row-kind="turn-fold"');
    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain('data-timeline-row-kind="work"');
    expect(html).toContain('data-bare="true"');
    expect(html).toContain('1 个工具');
    expect(html.indexOf('data-timeline-row-kind="turn-fold"'))
      .toBeLessThan(html.indexOf('data-timeline-row-kind="work"'));
  });
});
