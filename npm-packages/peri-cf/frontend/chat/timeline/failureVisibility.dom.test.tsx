import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../test/dom';

installDom();

const { cleanup, render, screen } = await import('@testing-library/react');
const { createElement } = await import('react');
const { deriveTimeline } = await import('./deriveTimeline');
const { TimelineRows } = await import('./Timeline');
const { WorkGroupRow } = await import('./rows/WorkGroupRow');
const { AssistantMessageRow } = await import('./rows/AssistantMessageRow');
const { ToolRow } = await import('./rows/ToolRow');
const { fireEvent, act } = await import('@testing-library/react');

afterEach(() => cleanup());

const chat = { id: 'chat-failure', title: '失败可见性', updatedAt: '2026-10-20T00:00:00Z' };

function toolEntry(options: {
  status: string; error?: { code: string; message: string } | null; result?: unknown;
}): unknown {
  return {
    entryId: 'turn:1:a', id: 'turn:1:a', kind: 'message', turnId: 'turn:1',
    role: 'assistant', status: options.status, createdAt: '2026-10-20T00:00:01Z',
    createdAtSource: 'observed', completedAt: '2026-10-20T00:00:02Z',
    error: null,
    blocks: [{
      blockId: 'tool:bash', type: 'tool_call', toolCallId: 'bash',
      tool: {
        toolCallId: 'bash', turnId: 'turn:1', name: 'Bash', status: options.status,
        createdAt: '2026-10-20T00:00:01Z', completedAt: '2026-10-20T00:00:02Z',
        error: options.error ?? null,
        arguments: { command: 'npm test' },
        ...(options.result === undefined ? {} : { result: options.result }),
      },
    }],
  };
}

function state(options: { turnStatus: string; entryStatus: string; error?: { code: string; message: string } | null; result?: unknown }): unknown {
  return {
    chat, sessionId: chat.id, bindingRevision: 1, running: false, executionBlocked: false, readiness: 'ready',
    entries: [{
      entryId: 'turn:1:user', id: 'turn:1:user', kind: 'message', turnId: 'turn:1',
      role: 'user', status: 'completed', createdAt: '2026-10-20T00:00:00Z',
      createdAtSource: 'observed', completedAt: null, error: null,
      blocks: [{ blockId: 'text:0', type: 'text', text: '跑一下测试' }],
    }, toolEntry({
      status: options.entryStatus, error: options.error, result: options.result,
    })],
    toolCalls: [],
    turns: [{
      turnId: 'turn:1', status: options.turnStatus,
      startedAt: '2026-10-20T00:00:00Z', completedAt: '2026-10-20T00:00:02Z',
      error: null, interactionStatus: 'idle',
    }],
    activeTurn: { turnId: null, status: null }, session: {},
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [],
    pendingInteractions: [], inputQueue: null,
  };
}

describe('failed and cancelled work is visible on the first screen', () => {
  test('a completed turn with a failed tool collapses first and reveals output when expanded', () => {
    const projection = state({
      turnStatus: 'completed', entryStatus: 'failed',
      // 真实投影形状：error 是公开文案，具体错误在 result.contentText，退出码可在文本标记里。
      error: { code: 'tool_failed', message: '工具执行失败' },
      result: { contentText: '[stderr]\npermission denied\n[Exit code: 1]' },
    }) as never;
    const collapsed = render(createElement(TimelineRows, { rows: deriveTimeline(projection), onToggleTurnFold: () => {} }));
    // fold 摘要显示失败，但工具细节默认收起。
    expect(screen.getByText('Failed after 2.0s')).toBeTruthy();
    expect(screen.queryByText('运行命令')).toBeNull();
    expect(screen.queryByText('permission denied · exit code 1')).toBeNull();
    collapsed.unmount();

    const rows = deriveTimeline(projection, { expandedTurnIds: new Set(['turn:1']) });
    render(createElement(TimelineRows, { rows, onToggleTurnFold: () => {} }));
    expect(screen.getByText('运行命令')).toBeTruthy();
    // 展开后保留真实错误文本与退出码；不使用通用 error.message。
    expect(screen.getByText('permission denied · exit code 1')).toBeTruthy();
    expect(screen.queryByText('工具执行失败')).toBeNull();

    const tool = document.querySelector('[data-slot="peri-tool-row"]')!;
    expect(tool.getAttribute('data-tool-failed')).toBe('true');
    const failure = document.querySelector('[data-slot="peri-tool-failure"]')!;
    expect(failure.className).toContain('text-destructive');
  });

  test('a cancelled turn collapses first and shows the neutral label when expanded', () => {
    const projection = state({
      turnStatus: 'cancelled', entryStatus: 'cancelled', result: { contentText: 'stopped' },
    }) as never;
    const collapsed = render(createElement(TimelineRows, { rows: deriveTimeline(projection), onToggleTurnFold: () => {} }));
    expect(screen.getByText('Cancelled after 2.0s')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-work-group"]')).toBeNull();
    collapsed.unmount();

    const rows = deriveTimeline(projection, { expandedTurnIds: new Set(['turn:1']) });
    render(createElement(TimelineRows, { rows, onToggleTurnFold: () => {} }));
    expect(screen.getByText('已取消')).toBeTruthy();
    expect(document.querySelector('[data-tool-failed="true"]')).toBeNull();
  });

  test('a clean expanded turn renders work items without a second toggle', () => {
    const rows = deriveTimeline(state({ turnStatus: 'completed', entryStatus: 'completed' }) as never, {
      expandedTurnIds: new Set(['turn:1']),
    });
    render(createElement(TimelineRows, { rows, onToggleTurnFold: () => {} }));

    expect(screen.getByText(/1 个工具/)).toBeTruthy();
    expect(screen.getByText('运行命令')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-work-group"] > button')).toBeNull();
    expect(
      document.querySelector('[data-slot="peri-work-group"]')?.getAttribute('data-bare'),
    ).toBe('true');
  });

  test('a group that turns failed after a manual collapse stays collapsed', async () => {
    const running = {
      kind: 'tool' as const, id: 'bash', entryId: 'turn:1:a', blockId: 'tool:bash',
      status: 'running', label: '运行命令', target: 'npm test',
      createdAt: null, completedAt: null, failed: false,
    };
    const failed = { ...running, status: 'failed', failed: true };
    const view = render(createElement(WorkGroupRow, {
      items: [running], active: true, failed: false, defaultExpanded: true,
    }));
    const button = () => document.querySelector('[data-slot="peri-work-group"] button')!;
    expect(button().getAttribute('aria-expanded')).toBe('true');
    fireEvent.click(button());
    expect(button().getAttribute('aria-expanded')).toBe('false');

    await act(async () => {
      view.rerender(createElement(WorkGroupRow, {
        items: [failed], active: false, failed: true, defaultExpanded: true,
      }));
    });
    expect(button().getAttribute('aria-expanded')).toBe('false');
    expect(document.querySelector('[data-work-failed="true"]')).toBeTruthy();
  });
});

describe('tool payload detail visibility', () => {
  test('an expanded failed tool loads the full result through payloadReader', async () => {
    const json = JSON.stringify({
      contentText: '[stderr]\npermission denied\nfull failure detail\n[Exit code: 3]',
      rawOutput: { exitCode: 3 },
    });
    const bytes = new TextEncoder().encode(json).byteLength;
    const item = {
      kind: 'tool', id: 'bash', entryId: 'turn:1:a', blockId: 'tool:bash',
      status: 'error', label: '运行命令', target: 'npm test',
      createdAt: null, completedAt: null, failed: true,
      tool: {
        toolCallId: 'bash', turnId: 'turn:1', name: 'Bash', status: 'error',
        createdAt: null, completedAt: null,
        error: { code: 'tool_failed', message: '工具执行失败' },
        arguments: { command: 'npm test' },
        resultRef: { id: 'payload-1', bytes, preview: '[stderr]\npermission denied' },
      },
    } as never;
    const payloadReader = async ({ id }: { id: string }) => ({ id, bytes, json });

    render(createElement(ToolRow, { item, payloadReader }));
    fireEvent.click(document.querySelector('[data-slot="peri-tool-row"] button')!);
    await act(async () => {
      fireEvent.click(screen.getByText('查看全文'));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    // 全文按清理后的 contentText 展示，而不是 JSON 引号转义或标记行。
    expect(screen.getByText(/full failure detail/)).toBeTruthy();
    const result = document.querySelector('[data-slot="peri-tool-result"]')!;
    expect(result.textContent).not.toContain('[stderr]');
    expect(result.textContent).not.toContain('"contentText"');
  });
});

describe('failure evidence and degraded payloads', () => {
  test('a completed turn with a failed tool starts collapsed with an enabled toggle', () => {
    const rows = deriveTimeline(state({
      turnStatus: 'completed', entryStatus: 'failed',
      error: { code: 'tool_failed', message: '工具执行失败' },
      result: { contentText: 'boom' },
    }) as never);
    render(createElement(TimelineRows, { rows, onToggleTurnFold: () => {} }));

    const fold = document.querySelector('[data-slot="peri-turn-fold"]')!;
    const toggle = fold.querySelector('button')!;
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(toggle.hasAttribute('disabled')).toBe(false);
    expect(screen.getByText('Failed after 2.0s')).toBeTruthy();
  });

  test('payload_too_large renders as a truncated completed result, not a failure', () => {
    const rows = deriveTimeline(state({
      turnStatus: 'completed', entryStatus: 'completed',
      error: { code: 'payload_too_large', message: '工具载荷超出会话接纳预算' },
      result: { contentText: 'partial output' },
    }) as never, { expandedTurnIds: new Set(['turn:1']) });
    render(createElement(TimelineRows, { rows, onToggleTurnFold: () => {} }));

    expect(screen.getByText('Worked for 2.0s')).toBeTruthy();
    const tool = document.querySelector('[data-slot="peri-tool-row"]')!;
    expect(tool.getAttribute('data-tool-failed')).toBe('false');
    expect(tool.getAttribute('data-tool-degraded')).toBe('true');
    expect(document.querySelector('[data-slot="peri-tool-failure"]')).toBeNull();
    expect(screen.getByText('结果超出会话接纳预算，已截断或省略。')).toBeTruthy();
  });
});

describe('row chrome', () => {
  test('an assistant message row renders no Peri label', () => {
    render(createElement(AssistantMessageRow, { text: '这是回答', streaming: false }));

    const row = document.querySelector('[data-slot="peri-assistant-message"]')!;
    expect(row.textContent).toContain('这是回答');
    expect(row.textContent).not.toContain('Peri');
    expect(screen.queryByText('Peri')).toBeNull();
  });

  test('a non-bare work group root draws no bottom border', () => {
    const item = {
      kind: 'reasoning' as const, id: 'r1', entryId: 'turn:1:a', blockId: 'r1',
      status: 'completed', label: '思考', text: '先检查一下',
      createdAt: null, completedAt: null, failed: false,
    };
    render(createElement(WorkGroupRow, {
      items: [item], active: false, failed: false, defaultExpanded: true,
    }));

    const group = document.querySelector('[data-slot="peri-work-group"]')!;
    expect(group.getAttribute('data-bare')).toBe('false');
    expect(group.classList.contains('border-b')).toBe(false);
    expect(group.className).not.toContain('border-b');
    expect(group.className).toContain('py-1');
    expect(screen.getByText('先检查一下')).toBeTruthy();
  });
});
