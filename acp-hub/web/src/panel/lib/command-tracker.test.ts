import { afterEach, describe, expect, it, vi } from 'vitest';
import { CommandTracker, type CommandRequest } from './command-tracker';

interface Frame { commandId: string; t: string; [key: string]: unknown }
interface Ack { commandId?: string; status?: string; chatId?: string; [key: string]: unknown }
interface ErrorFrame { commandId?: string; code?: string; message?: string }

function harness() {
  const fallback = vi.fn();
  const counts: number[] = [];
  const tracker = new CommandTracker<Frame, Ack, ErrorFrame>({
    timeoutMs: 30_000,
    onFallbackUncertain: fallback,
    onUncertainCountChange: (count) => counts.push(count),
  });
  return { tracker, fallback, counts };
}

afterEach(() => vi.useRealTimers());

describe('CommandTracker', () => {
  it('keeps accepted commands pending until one terminal acknowledgement', () => {
    const { tracker } = harness();
    const accepted = vi.fn();
    const terminal = vi.fn();
    const request: CommandRequest<Frame, Ack, ErrorFrame> = {
      frame: { t: 'action', commandId: 'cmd-1' },
      label: 'session/open',
      callbacks: { onAccepted: accepted, onTerminal: terminal },
    };
    expect(tracker.dispatch(request, () => true)).toBe('sent');
    expect(tracker.acknowledge({ commandId: 'cmd-1', status: 'accepted' })).toBe(true);
    expect(accepted).toHaveBeenCalledOnce();
    expect(tracker.hasPending('cmd-1')).toBe(true);
    expect(tracker.acknowledge({ commandId: 'cmd-1', status: 'committed' })).toBe(true);
    expect(terminal).toHaveBeenCalledOnce();
    expect(tracker.hasPending('cmd-1')).toBe(false);
    expect(tracker.acknowledge({ commandId: 'cmd-1', status: 'duplicate' })).toBe(false);
  });

  it('retains the exact frame for same-command retry after timeout', () => {
    vi.useFakeTimers();
    const { tracker, counts } = harness();
    const uncertain = vi.fn();
    const frame = { t: 'action', commandId: 'cmd-retry', payload: { projectId: 'p1' } };
    tracker.dispatch({ frame, label: 'project/create', callbacks: { retryOnUncertain: true, onUncertain: uncertain } }, () => true);
    vi.advanceTimersByTime(30_000);
    expect(uncertain).toHaveBeenCalledWith('timeout');
    expect(tracker.hasUncertain('cmd-retry')).toBe(true);
    const sent: Frame[] = [];
    expect(tracker.retry('cmd-retry', (candidate) => { sent.push(candidate); return true; })).toBe('sent');
    expect(sent).toEqual([frame]);
    expect(counts).toEqual([1]);
    tracker.acknowledge({ commandId: 'cmd-retry', status: 'duplicate' });
    expect(counts).toEqual([1, 0]);
  });

  it('drives an uncertain prompt back through the same lifecycle and settles one duplicate', () => {
    vi.useFakeTimers();
    const { tracker } = harness();
    const transitions: string[] = [];
    const frame = { t: 'action', commandId: 'prompt-stable', action: 'chat/prompt', payload: { chatId: 'chat-1', message: 'keep me' } };
    tracker.dispatch({
      frame,
      label: 'prompt',
      callbacks: {
        retryOnUncertain: true,
        onAccepted: () => transitions.push('accepted'),
        onUncertain: () => transitions.push('uncertain'),
        onTerminal: (ack) => transitions.push(`terminal:${ack.status}`),
      },
    }, () => true);
    tracker.acknowledge({ commandId: frame.commandId, status: 'accepted' });
    vi.advanceTimersByTime(30_000);

    const resent: Frame[] = [];
    expect(tracker.retry(frame.commandId, (candidate) => { resent.push(candidate); return true; })).toBe('sent');
    expect(resent).toEqual([frame]);
    expect(tracker.acknowledge({ commandId: frame.commandId, status: 'duplicate' })).toBe(true);
    expect(tracker.acknowledge({ commandId: frame.commandId, status: 'committed' })).toBe(false);
    expect(transitions).toEqual(['accepted', 'uncertain', 'terminal:duplicate']);
    expect(tracker.hasUncertain(frame.commandId)).toBe(false);
  });

  it('never turns a definite terminal error into same-command retry state', () => {
    const { tracker } = harness();
    const onError = vi.fn();
    tracker.dispatch({
      frame: { t: 'action', commandId: 'definite-failure', action: 'chat/prompt' },
      label: 'prompt',
      callbacks: { retryOnUncertain: true, onError },
    }, () => true);
    expect(tracker.fail({ commandId: 'definite-failure', code: 'INVALID_STATE' })).toBe(true);
    expect(onError).toHaveBeenCalledOnce();
    expect(tracker.hasUncertain('definite-failure')).toBe(false);
    expect(tracker.retry('definite-failure', () => true)).toBeNull();
  });

  it('forgets reconciliation state when a terminal acknowledgement arrives late', () => {
    vi.useFakeTimers();
    const { tracker, counts } = harness();
    const terminal = vi.fn();
    tracker.dispatch({
      frame: { t: 'action', commandId: 'late' },
      label: 'project/archive',
      callbacks: { retryOnUncertain: true, onTerminal: terminal },
    }, () => true);
    vi.advanceTimersByTime(30_000);
    expect(tracker.hasUncertain('late')).toBe(true);
    expect(tracker.acknowledge({ commandId: 'late', status: 'committed' })).toBe(false);
    expect(terminal).not.toHaveBeenCalled();
    expect(tracker.hasUncertain('late')).toBe(false);
    expect(counts).toEqual([1, 0]);
  });

  it('settles every pending command as uncertain on disconnect', () => {
    const { tracker, fallback } = harness();
    const custom = vi.fn();
    tracker.dispatch({ frame: { t: 'action', commandId: 'one' }, label: 'one', callbacks: { onUncertain: custom } }, () => true);
    tracker.dispatch({ frame: { t: 'action', commandId: 'two' }, label: 'two' }, () => true);
    tracker.settleConnectionLoss();
    expect(custom).toHaveBeenCalledWith('disconnect');
    expect(fallback).toHaveBeenCalledWith(expect.objectContaining({ label: 'two' }), 'disconnect');
    expect(tracker.hasPending('one')).toBe(false);
    expect(tracker.hasPending('two')).toBe(false);
  });

  it('reports transport rejection without registering a timer or callback', () => {
    vi.useFakeTimers();
    const { tracker, fallback } = harness();
    const error = vi.fn();
    expect(tracker.dispatch({ frame: { t: 'action', commandId: 'no-send' }, label: 'prompt', callbacks: { onError: error } }, () => false)).toBe('unavailable');
    vi.runAllTimers();
    expect(error).not.toHaveBeenCalled();
    expect(fallback).not.toHaveBeenCalled();
    expect(tracker.hasPending('no-send')).toBe(false);
  });

  it('rejects a second in-flight dispatch with the same command identity', () => {
    const { tracker } = harness();
    const send = vi.fn(() => true);
    const request = { frame: { t: 'action', commandId: 'same' }, label: 'prompt' };
    expect(tracker.dispatch(request, send)).toBe('sent');
    expect(tracker.dispatch(request, send)).toBe('already_pending');
    expect(send).toHaveBeenCalledOnce();
  });

  it('keeps reconciliation evidence when a retry cannot reach the transport', () => {
    vi.useFakeTimers();
    const { tracker, counts } = harness();
    tracker.dispatch({
      frame: { t: 'action', commandId: 'offline-retry' },
      label: 'session/import',
      callbacks: { retryOnUncertain: true },
    }, () => true);
    vi.advanceTimersByTime(30_000);
    expect(tracker.retry('offline-retry', () => false)).toBe('unavailable');
    expect(tracker.hasUncertain('offline-retry')).toBe(true);
    expect(counts).toEqual([1]);
  });

  it('routes a terminal error once and clears any pending timer', () => {
    vi.useFakeTimers();
    const { tracker, fallback } = harness();
    const onError = vi.fn();
    tracker.dispatch({ frame: { t: 'action', commandId: 'bad' }, label: 'prompt', callbacks: { onError } }, () => true);
    expect(tracker.fail({ commandId: 'bad', code: 'INVALID_STATE' })).toBe(true);
    vi.runAllTimers();
    expect(onError).toHaveBeenCalledOnce();
    expect(fallback).not.toHaveBeenCalled();
    expect(tracker.fail({ commandId: 'bad', code: 'INVALID_STATE' })).toBe(false);
  });

  it('reset cancels timers and clears pending and uncertain state without callbacks', () => {
    vi.useFakeTimers();
    const { tracker, fallback, counts } = harness();
    tracker.dispatch({
      frame: { t: 'action', commandId: 'reset-me' },
      label: 'project/rename',
      callbacks: { retryOnUncertain: true },
    }, () => true);
    tracker.reset();
    vi.runAllTimers();
    expect(tracker.hasPending('reset-me')).toBe(false);
    expect(tracker.hasUncertain('reset-me')).toBe(false);
    expect(fallback).not.toHaveBeenCalled();
    expect(counts).toEqual([0]);
  });
});
