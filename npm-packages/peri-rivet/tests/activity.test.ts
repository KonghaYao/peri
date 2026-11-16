import { describe, expect, test } from 'bun:test';
import { ActorError } from '../src/errors.js';
import {
  ActivityLedger,
  canReleaseActivation,
  isQuiescent,
  type RuntimeActivity,
  requiresActivationContext,
} from '../src/host/activity.js';

function activity(overrides: Partial<RuntimeActivity> = {}): RuntimeActivity {
  return {
    revision: 0,
    complete: true,
    rootRunning: false,
    activeInternalTasks: 0,
    queuedInputs: 0,
    pendingDeliveries: 0,
    pendingInteractions: 0,
    localOperations: 0,
    closing: false,
    ...overrides,
  };
}

describe('activity ledger', () => {
  test('counts leases and increments the revision on every transition', () => {
    const ledger = new ActivityLedger();
    expect(ledger.snapshot().revision).toBe(0);

    const root = ledger.beginRoot();
    const child = ledger.beginInternalTask();
    const delivery = ledger.beginDelivery();
    const interaction = ledger.beginInteraction();
    const local = ledger.beginLocalOperation();

    expect(ledger.snapshot()).toMatchObject({
      rootRunning: true,
      activeInternalTasks: 1,
      pendingDeliveries: 1,
      pendingInteractions: 1,
      localOperations: 1,
      revision: 5,
    });

    root.release();
    child.release();
    delivery.release();
    interaction.release();
    local.release();

    expect(isQuiescent(ledger.snapshot())).toBe(true);
    expect(ledger.snapshot().revision).toBe(10);
  });

  test('leases are idempotent and snapshots are frozen', () => {
    const ledger = new ActivityLedger();
    const lease = ledger.beginInternalTask();
    lease.release();
    lease.release();
    expect(lease.released).toBe(true);
    expect(ledger.snapshot().activeInternalTasks).toBe(0);
    expect(Object.isFrozen(ledger.snapshot())).toBe(true);
  });

  test('a second root lease is an invariant violation', () => {
    const ledger = new ActivityLedger();
    ledger.beginRoot();
    expect(() => ledger.beginRoot()).toThrow(ActorError);
  });

  test('closing rejects new external admission but allows finishing internal work', () => {
    const ledger = new ActivityLedger();
    const child = ledger.beginInternalTask();
    ledger.beginClose();

    expect(() => ledger.beginQueuedInput()).toThrow(ActorError);
    const allowedWhileClosing = ledger.beginInternalTask();
    allowedWhileClosing.release();

    child.release();
    const internal = ledger.beginInternalTask();
    internal.release();
    expect(isQuiescent(ledger.snapshot())).toBe(true);
  });

  test('quiescence waiters resolve once every lease is released', async () => {
    const ledger = new ActivityLedger();
    const lease = ledger.beginLocalOperation();
    let resolved = false;
    const waiting = ledger.waitForQuiescence().then(() => {
      resolved = true;
    });

    await Promise.resolve();
    expect(resolved).toBe(false);
    lease.release();
    await waiting;
    expect(resolved).toBe(true);
  });

  test('release eligibility requires a complete snapshot and an open unit', () => {
    expect(canReleaseActivation(activity())).toBe(true);
    expect(canReleaseActivation(activity({ complete: false }))).toBe(false);
    expect(canReleaseActivation(activity({ closing: true }))).toBe(false);
    expect(canReleaseActivation(activity({ activeInternalTasks: 1 }))).toBe(false);
  });

  test('requiresActivationContext keeps partial and closing snapshots alive', () => {
    expect(requiresActivationContext(activity({ complete: false }))).toBe(true);
    expect(requiresActivationContext(activity({ closing: true }))).toBe(true);
    expect(requiresActivationContext(activity({ pendingInteractions: 1 }))).toBe(true);
    expect(requiresActivationContext(activity())).toBe(false);
  });
});
