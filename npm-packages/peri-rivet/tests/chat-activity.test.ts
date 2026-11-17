import { describe, expect, test } from 'bun:test';
import type { ChatActivitySource, ChatActivitySourceSnapshot } from '@peri-code/sdk';
import { canReleaseActivation } from '../src/host/activity.js';
import { ChatActivityLedger } from '../src/host/chat-activity.js';
import { createChatObservation } from './helpers.js';

function completeSource(snapshot: ChatActivitySourceSnapshot): ChatActivitySource & {
  update(next: ChatActivitySourceSnapshot): void;
} {
  let current = snapshot;
  const listeners = new Set<() => void>();
  return {
    snapshot: () => current,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    update(next) {
      current = next;
      for (const listener of listeners) {
        listener();
      }
    },
  };
}

describe('chat activity ledger', () => {
  test('public SDK observation alone never releases the activation', () => {
    const activity = new ChatActivityLedger();
    activity.start();
    activity.applyObservation(createChatObservation());

    expect(activity.activity.rootRunning).toBe(false);
    expect(activity.activity.queuedInputs).toBe(0);
    expect(activity.activity.complete).toBe(false);
    expect(canReleaseActivation(activity.activity)).toBe(false);
  });

  test('a complete source releases only after internal tasks drain', () => {
    const source = completeSource({ complete: true, activeInternalTasks: 1, pendingDeliveries: 0 });
    const activity = new ChatActivityLedger({ source });
    activity.start();
    activity.applyObservation(createChatObservation());

    expect(activity.activity.activeInternalTasks).toBe(1);
    expect(canReleaseActivation(activity.activity)).toBe(false);

    source.update({ complete: true, activeInternalTasks: 0, pendingDeliveries: 0 });
    expect(canReleaseActivation(activity.activity)).toBe(true);
  });

  test('task snapshots count non-terminal tasks through the SDK tracker', () => {
    const activity = new ChatActivityLedger();
    activity.applyFact({
      kind: 'task-snapshot',
      snapshot: {
        revision: 2,
        tasks: [{ status: 'running' }, { status: 'unobserved:waiting' }, { status: 'completed' }],
      },
    });

    expect(activity.activity.activeInternalTasks).toBe(2);
  });

  test('a local operation and closing both pin the activation', () => {
    const source = completeSource({ complete: true, activeInternalTasks: 0, pendingDeliveries: 0 });
    const activity = new ChatActivityLedger({ source });
    activity.start();
    activity.applyObservation(createChatObservation());
    expect(canReleaseActivation(activity.activity)).toBe(true);

    const lease = activity.beginLocalOperation();
    expect(activity.activity.localOperations).toBe(1);
    expect(canReleaseActivation(activity.activity)).toBe(false);
    lease.release();
    expect(activity.activity.localOperations).toBe(0);

    activity.beginClose();
    expect(activity.activity.closing).toBe(true);
    expect(canReleaseActivation(activity.activity)).toBe(false);
  });

  test('malformed source counts are clamped instead of growing leases forever', () => {
    const source = completeSource({
      complete: true,
      activeInternalTasks: -1,
      pendingDeliveries: Number.NaN,
    });
    const activity = new ChatActivityLedger({ source });
    activity.start();

    expect(activity.activity.activeInternalTasks).toBe(0);
    expect(activity.activity.pendingDeliveries).toBe(0);
  });

  test('a counter handoff never publishes a releasable snapshot mid-update', () => {
    const source = completeSource({ complete: true, activeInternalTasks: 1, pendingDeliveries: 0 });
    const activity = new ChatActivityLedger({ source });
    activity.start();

    const published: boolean[] = [];
    activity.subscribe((snapshot) => published.push(canReleaseActivation(snapshot)));

    source.update({ complete: true, activeInternalTasks: 0, pendingDeliveries: 1 });

    expect(published).not.toContain(true);
    expect(canReleaseActivation(activity.activity)).toBe(false);
  });

  test('an unchanged refresh does not publish a new revision', () => {
    const source = completeSource({ complete: true, activeInternalTasks: 0, pendingDeliveries: 0 });
    const activity = new ChatActivityLedger({ source });
    activity.start();
    const revision = activity.activity.revision;

    source.update({ complete: true, activeInternalTasks: 0, pendingDeliveries: 0 });
    expect(activity.activity.revision).toBe(revision);
  });

  test('a failing source is reported and leaves the activation pinned', () => {
    const errors: unknown[] = [];
    const source: ChatActivitySource = {
      snapshot() {
        throw new Error('source unavailable');
      },
    };
    const activity = new ChatActivityLedger({
      source,
      onObserverError: (error) => errors.push(error),
    });
    activity.start();

    expect(errors).toHaveLength(1);
    expect(activity.activity.complete).toBe(false);
    expect(canReleaseActivation(activity.activity)).toBe(false);
  });

  test('markClosed clears counters while keeping local operations', () => {
    const source = completeSource({ complete: true, activeInternalTasks: 2, pendingDeliveries: 0 });
    const activity = new ChatActivityLedger({ source });
    activity.start();
    const lease = activity.beginLocalOperation();

    activity.markClosed();

    expect(activity.activity.activeInternalTasks).toBe(0);
    expect(activity.activity.closing).toBe(true);
    expect(activity.activity.localOperations).toBe(1);
    expect(canReleaseActivation(activity.activity)).toBe(false);

    lease.release();
    expect(activity.activity.localOperations).toBe(0);
  });
});
