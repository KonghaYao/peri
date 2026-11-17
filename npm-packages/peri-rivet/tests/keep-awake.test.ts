import { describe, expect, test } from 'bun:test';
import { ActivityLedger } from '../src/host/activity.js';
import {
  KeepAwakeCoordinator,
  type KeepAwakeHold,
  type KeepAwakePort,
  promiseKeepAwakePort,
} from '../src/host/keep-awake.js';
import { createRecordingKeepAwake, sleep } from './helpers.js';

describe('keep-awake coordinator', () => {
  test('holds one activation lease across overlapping work and releases at quiescence', () => {
    const ledger = new ActivityLedger();
    const recording = createRecordingKeepAwake();
    const coordinator = new KeepAwakeCoordinator(ledger, recording.port, { onError: () => {} });
    coordinator.start();

    const first = ledger.beginInternalTask();
    const second = ledger.beginDelivery();
    expect(recording.active).toBe(1);
    expect(recording.acquires).toBe(1);
    expect(coordinator.holding).toBe(true);

    first.release();
    expect(recording.active).toBe(1);
    second.release();
    expect(recording.active).toBe(0);
    expect(recording.releases).toBe(1);
    expect(coordinator.holding).toBe(false);

    coordinator.stop();
  });

  test('closing keeps the activation alive until the coordinator stops', () => {
    const ledger = new ActivityLedger();
    const recording = createRecordingKeepAwake();
    const coordinator = new KeepAwakeCoordinator(ledger, recording.port, { onError: () => {} });
    coordinator.start();

    ledger.beginClose();
    expect(recording.active).toBe(1);

    coordinator.stop();
    expect(recording.active).toBe(0);
  });

  test('acquisition failures are reported and retried on the next change', () => {
    const ledger = new ActivityLedger();
    const errors: unknown[] = [];
    let attempts = 0;
    const flaky: KeepAwakePort = {
      acquire(): KeepAwakeHold {
        attempts += 1;
        if (attempts === 1) {
          throw new Error('keep-awake backend unavailable');
        }
        let released = false;
        return {
          release() {
            if (released) {
              return;
            }
            released = true;
          },
        };
      },
    };
    const coordinator = new KeepAwakeCoordinator(ledger, flaky, {
      onError: (error) => errors.push(error),
    });
    coordinator.start();

    const first = ledger.beginInternalTask();
    expect(errors).toHaveLength(1);
    expect(coordinator.holding).toBe(false);

    const second = ledger.beginInternalTask();
    expect(errors).toHaveLength(1);
    expect(attempts).toBe(2);
    expect(coordinator.holding).toBe(true);

    first.release();
    second.release();
    expect(coordinator.holding).toBe(false);
  });

  test('the promise adapter resolves the keep-awake promise on release', async () => {
    const promises: Promise<void>[] = [];
    const port = promiseKeepAwakePort((promise) => {
      promises.push(promise);
    });
    const hold = port.acquire();
    expect(promises).toHaveLength(1);

    let released = false;
    void promises[0]?.then(() => {
      released = true;
    });
    await sleep(0);
    expect(released).toBe(false);

    hold.release();
    hold.release();
    await promises[0];
    expect(released).toBe(true);
  });
});
