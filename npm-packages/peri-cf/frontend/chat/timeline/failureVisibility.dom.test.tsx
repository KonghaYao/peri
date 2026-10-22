import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../test/dom';

installDom();

const { cleanup, render, screen } = await import('@testing-library/react');
const { createElement } = await import('react');
const { deriveTimeline } = await import('./deriveTimeline');
const { TimelineRows } = await import('./Timeline');
const { WorkGroupRow } = await import('./rows/WorkGroupRow');
const { ToolRow } = await import('./rows/ToolRow');
const { ChatProjection } = await import('@core/agent');
const { fireEvent, act } = await import('@testing-library/react');

afterEach(() => cleanup());

const chat = { id: 'chat-failure', title: '失败可见性', updatedAt: '2026-10-20T00:00:00Z' };

function notification(update: Record<string, unknown>) {
  return {
    jsonrpc: '2.0' as const,
    method: 'session/update',
    params: { sessionId: chat.id, update },
  };
}

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
    chat, running: false, executionBlocked: false, readiness: 'ready', revision: 1,
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
  test('a completed turn with a failed tool shows the failure label, real output and exit code', () => {
    const rows = deriveTimeline(state({
      turnStatus: 'completed', entryStatus: 'failed',
      // 真实投影形状：error 是公开文案，具体错误在 result.contentText，退出码可在文本标记里。
      error: { code: 'tool_failed', message: '工具执行失败' },
      result: { contentText: '[stderr]\npermission denied\n[Exit code: 1]' },
    }) as never);
    render(createElement(TimelineRows, { rows }));

    // fold 摘要不再只显示 Worked for X。
    expect(screen.getByText('Failed after 2.0s')).toBeTruthy();
    expect(screen.getByText('失败 1')).toBeTruthy();
    expect(screen.getByText('运行命令')).toBeTruthy();
    // 首屏保留真实错误文本与退出码；不使用通用 error.message。
    expect(screen.getByText('permission denied · exit code 1')).toBeTruthy();
    expect(screen.queryByText('工具执行失败')).toBeNull();

    const tool = document.querySelector('[data-slot="peri-tool-row"]')!;
    expect(tool.getAttribute('data-tool-failed')).toBe('true');
    const failure = document.querySelector('[data-slot="peri-tool-failure"]')!;
    expect(failure.className).toContain('text-destructive');
    // 失败组默认展开：失败工具行首屏可见。
    const group = document.querySelector('[data-slot="peri-work-group"]')!;
    expect(group.querySelector('button')?.getAttribute('aria-expanded')).toBe('true');
  });

  test('a cancelled turn keeps its work expanded with the neutral cancelled label', () => {
    const rows = deriveTimeline(state({
      turnStatus: 'cancelled', entryStatus: 'cancelled', result: { contentText: 'stopped' },
    }) as never);
    render(createElement(TimelineRows, { rows }));

    expect(screen.getByText('Cancelled after 2.0s')).toBeTruthy();
    const group = document.querySelector('[data-slot="peri-work-group"]')!;
    expect(group.querySelector('button')?.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByText('已取消')).toBeTruthy();
    expect(document.querySelector('[data-tool-failed="true"]')).toBeNull();
  });

  test('a group that turns failed after a manual collapse expands again', async () => {
    const running = {
      kind: 'tool' as const, id: 'bash', entryId: 'turn:1:a', blockId: 'tool:bash',
      status: 'running', label: '运行命令', target: 'npm test',
      createdAt: null, completedAt: null, failed: false,
    };
    const failed = { ...running, status: 'failed', failed: true };
    const view = render(createElement(WorkGroupRow, {
      items: [running], active: true, failed: false, defaultExpanded: true, forceExpanded: false,
    }));
    const button = () => document.querySelector('[data-slot="peri-work-group"] button')!;
    expect(button().getAttribute('aria-expanded')).toBe('true');
    fireEvent.click(button());
    expect(button().getAttribute('aria-expanded')).toBe('false');

    await act(async () => {
      view.rerender(createElement(WorkGroupRow, {
        items: [failed], active: false, failed: true, defaultExpanded: true, forceExpanded: true,
      }));
    });
    expect(button().getAttribute('aria-expanded')).toBe('true');
    expect(document.querySelector('[data-work-failed="true"]')).toBeTruthy();
  });
});

describe('failure visibility over the real ChatProjection', () => {
  test('a failed tool shows its real stderr text and exit code on the first screen', async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    await projection.start('跑一下测试');
    projection.accept(notification({
      sessionUpdate: 'tool_call', toolCallId: 'bash-real', title: 'Bash', status: 'in_progress',
      rawInput: { command: 'npm test' },
    }));
    projection.accept(notification({
      sessionUpdate: 'tool_call_update', toolCallId: 'bash-real', status: 'failed',
      content: [{ type: 'content', content: { type: 'text', text: '[stderr]\ncommand not found\n[Exit code: 127]' } }],
    }));
    await projection.complete('error');
    projection.setRunState({ chat, running: false, executionBlocked: false });

    const state = projection.state();
    const tool = state.toolCalls.find((item) => item.toolCallId === 'bash-real');
    // 真实投影：error 只有公开文案，具体错误与退出码在 contentText 的标记文本里。
    expect(tool?.error).toEqual({ code: 'tool_failed', message: '工具执行失败' });
    expect(tool?.result).toEqual({ contentText: '[stderr]\ncommand not found\n[Exit code: 127]' });

    render(createElement(TimelineRows, { rows: deriveTimeline(state) }));
    expect(screen.getByText(/Failed after/)).toBeTruthy();
    expect(screen.getByText('command not found · exit code 127')).toBeTruthy();
    expect(screen.queryByText('工具执行失败')).toBeNull();
    const failure = document.querySelector('[data-slot="peri-tool-failure"]')!;
    expect(failure.textContent).not.toContain('[stderr]');
    expect(document.querySelector('[data-tool-failed="true"]')).toBeTruthy();
    projection.dispose();
  });

  test('a failed JSON tool result surfaces stderr instead of the raw object', async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    await projection.start('跑一下测试');
    projection.accept(notification({
      sessionUpdate: 'tool_call', toolCallId: 'bash-json', title: 'Bash', status: 'in_progress',
      rawInput: { command: 'npm test' },
    }));
    projection.accept(notification({
      sessionUpdate: 'tool_call_update', toolCallId: 'bash-json', status: 'failed',
      content: [{
        type: 'content',
        content: { type: 'text', text: '{"stdout":"","stderr":"permission denied","exitCode":1}' },
      }],
    }));
    await projection.complete('error');
    projection.setRunState({ chat, running: false, executionBlocked: false });

    const state = projection.state();
    const tool = state.toolCalls.find((item) => item.toolCallId === 'bash-json');
    expect(tool?.result).toEqual({
      contentText: '{"stdout":"","stderr":"permission denied","exitCode":1}',
    });

    render(createElement(TimelineRows, { rows: deriveTimeline(state) }));
    expect(screen.getByText('permission denied · exit code 1')).toBeTruthy();
    const failure = document.querySelector('[data-slot="peri-tool-failure"]')!;
    expect(failure.textContent).not.toContain('{');
    expect(failure.textContent).not.toContain('stdout');
    projection.dispose();
  });

  test('a failure in a later work group colors the whole-turn fold and stays visible', async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    await projection.start('先读再跑');
    projection.accept(notification({
      sessionUpdate: 'tool_call', toolCallId: 'read-1', title: 'Read', status: 'in_progress',
      rawInput: { file_path: '/workspace/README.md' },
    }));
    projection.accept(notification({
      sessionUpdate: 'tool_call_update', toolCallId: 'read-1', status: 'completed',
      content: [{ type: 'content', content: { type: 'text', text: 'ok' } }],
    }));
    projection.accept(notification({
      sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: '中间回答' },
    }));
    projection.accept(notification({
      sessionUpdate: 'tool_call', toolCallId: 'bash-late', title: 'Bash', status: 'in_progress',
      rawInput: { command: 'npm test' },
    }));
    projection.accept(notification({
      sessionUpdate: 'tool_call_update', toolCallId: 'bash-late', status: 'failed',
      content: [{ type: 'content', content: { type: 'text', text: '[stderr]\npermission denied\n[Exit code: 2]' } }],
    }));
    await projection.complete('completed');
    projection.setRunState({ chat, running: false, executionBlocked: false });

    const rows = deriveTimeline(projection.state());
    const fold = rows.find((row) => row.kind === 'turn-fold');
    expect(fold).toMatchObject({ kind: 'turn-fold', failed: true });
    expect(fold?.kind === 'turn-fold' ? fold.label : '').toMatch(/^Failed after/);
    // 失败工具位于第二个 work 组，仍必须在首屏可见。
    const failedWork = rows.find((row) => row.kind === 'work' && row.items.some((item) => item.failed));
    expect(failedWork).toBeDefined();

    render(createElement(TimelineRows, { rows }));
    expect(screen.getByText(/Failed after/)).toBeTruthy();
    expect(document.querySelector('[data-turn-failed="true"]')).toBeTruthy();
    expect(screen.getByText('permission denied · exit code 2')).toBeTruthy();
    expect(document.querySelector('[data-work-failed="true"]')).toBeTruthy();
    expect(document.querySelector('[data-tool-failed="true"]')).toBeTruthy();
    projection.dispose();
  });

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
