import { describe, expect, test } from 'bun:test';
import { ChatProjection } from '@core/agent';
import { deriveTimeline } from './deriveTimeline';

const chat = { id: 'session-1', title: '投影时间线', updatedAt: '2026-10-20T00:00:00Z' };

function notification(update: Record<string, unknown>) {
  return {
    jsonrpc: '2.0' as const,
    method: 'session/update',
    params: { sessionId: chat.id, update },
  };
}

function setRunning(projection: ChatProjection, running: boolean): void {
  projection.setRunState({ chat, running, executionBlocked: false });
}

/** 真实投影路径：启动 turn、写入工具调用，再按给定终态结算。 */
async function settleTurn(status: 'completed' | 'cancelled' | 'error'): Promise<ChatProjection> {
  const projection = new ChatProjection();
  setRunning(projection, true);
  await projection.start('读一下 README');
  projection.accept(notification({
    sessionUpdate: 'tool_call', toolCallId: 'tool-1', title: 'Read', status: 'in_progress',
    rawInput: { file_path: '/workspace/README.md' },
  }));
  if (status === 'completed') {
    projection.accept(notification({
      sessionUpdate: 'tool_call_update', toolCallId: 'tool-1', status: 'completed',
      rawOutput: { content: 'ok' },
    }));
  }
  await projection.complete(status);
  setRunning(projection, false);
  return projection;
}

describe('deriveTimeline over the real ChatProjection lifecycle', () => {
  test('a completed turn folds immediately even though activeTurnId is not cleared', async () => {
    const projection = await settleTurn('completed');
    const state = projection.state();
    // TurnMachine.exit 保留 activeTurnId，但 running 已为 false：live 判定不能只看 id。
    expect(state.activeTurn.turnId).not.toBeNull();
    expect(state.running).toBe(false);
    expect(state.turns.at(-1)?.status).toBe('completed');
    const turnId = state.turns.at(-1)!.turnId;

    const rows = deriveTimeline(state);
    const fold = rows.find((row) => row.kind === 'turn-fold');
    expect(fold).toBeDefined();
    expect(fold).toMatchObject({ kind: 'turn-fold', turnId, status: 'completed', expanded: false });
    expect(fold?.label).toContain('Worked for');
    expect(rows.some((row) => row.kind === 'work')).toBe(false);
    projection.dispose();
  });

  test('the next running turn keeps its own work expanded while the previous turn stays folded', async () => {
    const projection = await settleTurn('completed');
    setRunning(projection, true);
    await projection.start('下一轮');
    projection.accept(notification({
      sessionUpdate: 'tool_call', toolCallId: 'tool-2', title: 'Read', status: 'in_progress',
      rawInput: { file_path: '/workspace/CHANGELOG.md' },
    }));
    // SessionViewStore 用微任务合并文档变更；真实订阅者也在下一帧读到同一快照。
    await projection.flush();

    const rows = deriveTimeline(projection.state());
    expect(rows.filter((row) => row.kind === 'turn-fold')).toHaveLength(1);
    const turns = projection.state().turns;
    const liveTurnId = turns.at(-1)!.turnId;
    expect(turns.at(-1)?.status).not.toBe('completed');
    const liveWork = rows.find((row) => row.kind === 'work' && row.turnId === liveTurnId);
    expect(liveWork).toBeDefined();
    expect(rows.some((row) => row.kind === 'working' && row.turnId === liveTurnId)).toBe(true);
    projection.dispose();
  });

  test('failed turns stay expanded with a failure summary and visible tool target', async () => {
    const projection = await settleTurn('error');
    const rows = deriveTimeline(projection.state());
    const fold = rows.find((row) => row.kind === 'turn-fold');
    expect(fold).toMatchObject({ kind: 'turn-fold', status: 'error', expanded: true });
    expect(fold?.label).toContain('Failed');
    const work = rows.find((row) => row.kind === 'work');
    expect(work).toBeDefined();
    expect(work?.kind === 'work' ? work.items[0]?.target : undefined).toBe('/workspace/README.md');
    projection.dispose();
  });

  test('cancelled turns stay expanded with a cancellation summary', async () => {
    const projection = await settleTurn('cancelled');
    const rows = deriveTimeline(projection.state());
    const fold = rows.find((row) => row.kind === 'turn-fold');
    expect(fold).toMatchObject({ kind: 'turn-fold', status: 'cancelled', expanded: true });
    expect(fold?.label).toContain('Cancelled');
    expect(rows.some((row) => row.kind === 'work')).toBe(true);
    projection.dispose();
  });
});
