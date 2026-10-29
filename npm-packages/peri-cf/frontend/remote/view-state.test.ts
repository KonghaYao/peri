import { describe, expect, test } from 'bun:test';
import type { ChatDisplayState } from './display-state';
import {
  bindingTargetOf, deriveLiveSync, deriveLoadState, derivePhase, describeActionFailure,
  isCancelPending, messageOf, rawErrorOf,
  type LoadStateInput, type PhaseInput,
} from './view-state';
import { RemoteActionError } from './actions';

function displayState(overrides: Partial<ChatDisplayState> = {}): ChatDisplayState {
  return {
    chat: { id: 'chat-1', title: '会话', updatedAt: '2026-10-20T00:00:00Z' },
    sessionId: 'chat-1', bindingRevision: 1, running: false, executionBlocked: false, readiness: 'ready',
    entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null }, session: {},
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [], pendingInteractions: [], inputQueue: null,
    ...overrides,
  };
}

const loadInput = (overrides: Partial<LoadStateInput> = {}): LoadStateInput => ({
  hasActive: true,
  hasSelection: true,
  readiness: 'not-ready',
  everReady: false,
  previous: 'idle',
  liveSync: 'connecting',
  connectionError: '',
  ...overrides,
});

const phaseInput = (overrides: Partial<PhaseInput> = {}): PhaseInput => ({
  hasActive: true,
  loadState: 'ready',
  uncertain: false,
  executionBlocked: false,
  recovery: 'none',
  running: false,
  cancelPending: false,
  historyReady: true,
  ...overrides,
});

describe('deriveLiveSync', () => {
  test('synced replica wins over a stale socket status', () => {
    expect(deriveLiveSync({ projectionStatus: 'synced', syncStatus: 'failed' })).toBe('synced');
  });

  test('either replica or socket failure is a failed read surface', () => {
    expect(deriveLiveSync({ projectionStatus: 'failed', syncStatus: 'connecting' })).toBe('failed');
    expect(deriveLiveSync({ projectionStatus: 'connecting', syncStatus: 'failed' })).toBe('failed');
  });

  test('no active replica stays connecting', () => {
    expect(deriveLiveSync({})).toBe('connecting');
  });
});

describe('deriveLoadState', () => {
  test('no active replica: selected chat is loading, empty shell is idle', () => {
    expect(deriveLoadState(loadInput({ hasActive: false, hasSelection: true }))).toBe('loading');
    expect(deriveLoadState(loadInput({ hasActive: false, hasSelection: false }))).toBe('idle');
  });

  test('replica readiness is authoritative', () => {
    expect(deriveLoadState(loadInput({ readiness: 'ready' }))).toBe('ready');
    expect(deriveLoadState(loadInput({ readiness: 'unsupported' }))).toBe('unsupported');
  });

  test('a replica that was ready keeps readable history after a disconnect', () => {
    expect(deriveLoadState(loadInput({ everReady: true, liveSync: 'failed' }))).toBe('ready');
  });

  test('unsupported never degrades back to ready after a reconnect', () => {
    expect(deriveLoadState(loadInput({ everReady: true, previous: 'unsupported' }))).toBe('unsupported');
  });

  test('first connection failure is failed, otherwise loading', () => {
    expect(deriveLoadState(loadInput({ liveSync: 'failed' }))).toBe('failed');
    expect(deriveLoadState(loadInput({ connectionError: 'socket closed' }))).toBe('failed');
    expect(deriveLoadState(loadInput())).toBe('loading');
  });
});

describe('derivePhase', () => {
  test('no active replica is idle regardless of other facts', () => {
    expect(derivePhase(phaseInput({ hasActive: false, uncertain: true, running: true }))).toBe('idle');
  });

  test('loading wins before blocked facts', () => {
    expect(derivePhase(phaseInput({ loadState: 'loading', uncertain: true }))).toBe('loading');
  });

  test('unknown result and execution block both block input', () => {
    expect(derivePhase(phaseInput({ uncertain: true }))).toBe('blocked');
    expect(derivePhase(phaseInput({ executionBlocked: true }))).toBe('blocked');
  });

  test('a reload recovery blocks an otherwise ready chat', () => {
    expect(derivePhase(phaseInput({ recovery: 'reload' }))).toBe('blocked');
    // 读面未就绪时仍按同步/失败处理，而不是提前阻断。
    expect(derivePhase(phaseInput({ recovery: 'reload', loadState: 'failed', historyReady: false }))).toBe('idle');
  });

  test('running turns distinguish streaming from a requested cancel', () => {
    expect(derivePhase(phaseInput({ running: true }))).toBe('streaming');
    expect(derivePhase(phaseInput({ running: true, cancelPending: true }))).toBe('cancelling');
  });

  test('pending history sync is syncing unless the read surface already failed', () => {
    expect(derivePhase(phaseInput({ historyReady: false }))).toBe('syncing');
    expect(derivePhase(phaseInput({ historyReady: false, loadState: 'failed' }))).toBe('idle');
    expect(derivePhase(phaseInput({ historyReady: false, loadState: 'unsupported' }))).toBe('idle');
  });
});

describe('isCancelPending', () => {
  const running = displayState({ running: true, activeTurn: { turnId: 'turn-1', status: 'running' } });

  test('no request or no running turn releases the intent', () => {
    expect(isCancelPending({ requested: false, turnId: 'turn-1' }, running)).toBe(false);
    expect(isCancelPending({ requested: true, turnId: 'turn-1' }, displayState())).toBe(false);
    expect(isCancelPending({ requested: true, turnId: 'turn-1' }, null)).toBe(false);
  });

  test('the same or unbound turn keeps the intent', () => {
    expect(isCancelPending({ requested: true, turnId: 'turn-1' }, running)).toBe(true);
    expect(isCancelPending({ requested: true, turnId: null }, running)).toBe(true);
  });

  test('a new turn invalidates the previous intent', () => {
    expect(isCancelPending({ requested: true, turnId: 'turn-1' }, displayState({
      running: true, activeTurn: { turnId: 'turn-2', status: 'running' },
    }))).toBe(false);
  });
});

describe('bindingTargetOf', () => {
  test('an unbound or malformed replica has no target', () => {
    expect(bindingTargetOf(null)).toBeNull();
    expect(bindingTargetOf(displayState({ sessionId: null }))).toBeNull();
    expect(bindingTargetOf(displayState({ bindingRevision: null }))).toBeNull();
    expect(bindingTargetOf(displayState({ bindingRevision: -1 }))).toBeNull();
  });

  test('a bound replica exposes the plain-data target', () => {
    expect(bindingTargetOf(displayState({ sessionId: 'session-9', bindingRevision: 4 })))
      .toEqual({ sessionId: 'session-9', bindingRevision: 4 });
  });
});

describe('describeActionFailure', () => {
  test('a binding conflict exposes reload and a rebind explanation', () => {
    const failure = describeActionFailure(
      new RemoteActionError('binding_conflict', 'Chat binding is stale', 'rejected'),
    );
    expect(failure.recovery).toBe('reload');
    expect(failure.uncertain).toBe(false);
    expect(failure.message).toContain('Chat binding is stale');
    expect(failure.message).toContain('会话绑定已过期');
  });

  test('an unknown disposition is uncertain and needs a reload', () => {
    const failure = describeActionFailure(
      new RemoteActionError('transport_error', '网络请求失败', 'unknown'),
    );
    expect(failure.recovery).toBe('reload');
    expect(failure.uncertain).toBe(true);
    expect(failure.message).toContain('执行状态尚未确认');
  });

  test('close_incomplete keeps its prefix and the unknown suffix', () => {
    const failure = describeActionFailure(new RemoteActionError(
      'close_incomplete', 'cleanup incomplete', 'unknown', { incomplete: true },
    ));
    expect(failure.message.startsWith('关闭未完成：cleanup incomplete')).toBe(true);
    expect(failure.message).toContain('执行状态尚未确认');
  });

  test('a plain rejection has no recovery action', () => {
    const failure = describeActionFailure(new RemoteActionError('validation_error', '内容不合法', 'rejected'));
    expect(failure).toEqual({ message: '内容不合法', recovery: 'none', uncertain: false });
  });

  test('a non-Error value is normalized to an unknown action failure', () => {
    const failure = describeActionFailure('boom');
    expect(failure.uncertain).toBe(true);
    expect(failure.message).toContain('boom');
    expect(messageOf({ toString: () => 'value' })).toBe('value');
  });
});

describe('rawErrorOf', () => {
  test('action errors win over connection errors', () => {
    expect(rawErrorOf({ actionError: '动作失败', connectionError: '连接失败', loadState: 'ready' })).toBe('动作失败');
  });

  test('an unsupported schema is only used when nothing else failed', () => {
    expect(rawErrorOf({ actionError: '', connectionError: '', loadState: 'unsupported' })).toContain('不受支持');
    expect(rawErrorOf({ actionError: '', connectionError: '', loadState: 'ready' })).toBe('');
  });
});
