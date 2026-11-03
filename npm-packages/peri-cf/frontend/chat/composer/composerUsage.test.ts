import { describe, expect, test } from 'bun:test';
import type { ChatDisplayState } from '../runtime';
import { readContextUsage } from './composerUsage';

function state(input: {
  entries?: ChatDisplayState['entries'];
  session?: ChatDisplayState['session'];
} = {}): ChatDisplayState {
  return {
    chat: { id: 'chat-1', title: 'Chat', updatedAt: '2026-10-20T00:00:00Z' },
    sessionId: 'chat-1', bindingRevision: 1, running: false, executionBlocked: false,
    readiness: 'ready', entries: input.entries ?? [], toolCalls: [], turns: [],
    activeTurn: { turnId: null, status: null }, session: input.session ?? {},
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [],
    pendingInteractions: [], inputQueue: null,
  };
}

function assistant(usage: Record<string, unknown> | undefined): ChatDisplayState['entries'][number] {
  return {
    entryId: 'e1', id: 'e1', kind: 'message', turnId: 't1', role: 'assistant',
    status: 'completed', createdAt: null, createdAtSource: 'observed', completedAt: null,
    error: null, tokenUsage: usage, blocks: [{ blockId: 'b1', type: 'text', text: 'ok' }],
  };
}

describe('readContextUsage', () => {
  test('prefers the latest assistant token usage and computes percent', () => {
    const usage = readContextUsage(state({
      entries: [
        assistant({ totalTokens: 10, contextWindow: 100 }),
        assistant({ totalTokens: 50, contextWindow: 200, inputTokens: 30, outputTokens: 20 }),
      ],
    }));
    expect(usage).toEqual({ used: 50, max: 200, percent: 25, totalProcessed: 50 });
  });

  test('falls back to stateSnapshotMeta budget percentage without inventing a window', () => {
    const usage = readContextUsage(state({
      session: { stateSnapshotMeta: { context_total_tokens: 12_000, budgetPct: 0.4 } },
    }));
    expect(usage).toEqual({ used: 12_000, max: null, percent: 40 });
  });

  test('ignores zero, NaN and malformed usage', () => {
    expect(readContextUsage(null)).toBeNull();
    expect(readContextUsage(state({ entries: [assistant({ totalTokens: 0, contextWindow: 100 })] }))).toBeNull();
    expect(readContextUsage(state({ entries: [assistant({ totalTokens: 10, contextWindow: 0 })] }))).toBeNull();
    expect(readContextUsage(state({ entries: [assistant({ totalTokens: Number.NaN, contextWindow: 100 })] }))).toBeNull();
    expect(readContextUsage(state({ session: { stateSnapshotMeta: { context_total_tokens: 0, budgetPct: 0 } } }))).toBeNull();
  });

  test('clamps percent and ignores out-of-range budgets', () => {
    const clamped = readContextUsage(state({
      entries: [assistant({ totalTokens: 300, contextWindow: 200 })],
    }));
    expect(clamped?.percent).toBe(100);

    expect(readContextUsage(state({
      session: { stateSnapshotMeta: { context_total_tokens: 10, budgetPct: 1.2 } },
    }))).toBeNull();
  });
});
