import { describe, expect, test } from 'bun:test';
import type { EntryDTO } from './contracts';
import type { ChatDisplayState } from './display-state';
import {
  CONNECTION_RELEASED_MESSAGE, DeliveryTracker, INPUT_WITHDRAWN_MESSAGE, deliveryFactOf,
} from './delivery';

function displayState(overrides: Partial<ChatDisplayState> = {}): ChatDisplayState {
  return {
    chat: { id: 'chat-1', title: '会话', updatedAt: '2026-10-20T00:00:00Z' },
    sessionId: 'chat-1', bindingRevision: 1, running: false, executionBlocked: false, readiness: 'ready',
    entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null }, session: {},
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [], pendingInteractions: [], inputQueue: null,
    ...overrides,
  };
}

function queue(items: { inputId: string; state: string }[]): ChatDisplayState {
  return displayState({ inputQueue: { generation: 'g1', revision: 1, items } });
}

function userEntry(entryId: string): EntryDTO {
  return {
    entryId, id: entryId, kind: 'message', turnId: 'turn-1', role: 'user', status: 'completed',
    createdAt: '2026-10-20T00:00:00Z', createdAtSource: 'observed', completedAt: null, error: null, blocks: [],
  };
}

describe('deliveryFactOf', () => {
  test('an empty replica keeps every input pending', () => {
    expect(deliveryFactOf(displayState(), 'input-1')).toBe('pending');
  });

  test('the queue settles delivered and withdrawn inputs', () => {
    expect(deliveryFactOf(queue([{ inputId: 'input-1', state: 'delivered' }]), 'input-1')).toBe('delivered');
    expect(deliveryFactOf(queue([{ inputId: 'input-1', state: 'withdrawn' }]), 'input-1')).toBe('withdrawn');
  });

  test('another input in the queue does not settle this one', () => {
    expect(deliveryFactOf(queue([{ inputId: 'input-2', state: 'delivered' }]), 'input-1')).toBe('pending');
  });

  test('a delivered user entry confirms the input even when the queue says withdrawn', () => {
    const state = displayState({
      entries: [userEntry('turn:input-1:user')],
      inputQueue: { generation: 'g1', revision: 1, items: [{ inputId: 'input-1', state: 'withdrawn' }] },
    });
    expect(deliveryFactOf(state, 'input-1')).toBe('delivered');
  });

  test('only the matching user entry counts as delivery', () => {
    expect(deliveryFactOf(displayState({ entries: [userEntry('turn:input-2:user')] }), 'input-1')).toBe('pending');
  });
});

describe('DeliveryTracker', () => {
  test('wait resolves only after the replica reports delivery', async () => {
    let state = displayState();
    const tracker = new DeliveryTracker(() => state);
    let settled = '';
    const waiting = tracker.wait('input-1').then(() => { settled = 'delivered'; });
    await Promise.resolve();
    expect(settled).toBe('');
    tracker.settle(queue([{ inputId: 'input-1', state: 'delivered' }]));
    await waiting;
    expect(settled).toBe('delivered');
  });

  test('a delivery fact that precedes the receipt resolves immediately', async () => {
    let state = queue([{ inputId: 'input-1', state: 'delivered' }]);
    const tracker = new DeliveryTracker(() => state);
    await expect(tracker.wait('input-1')).resolves.toBeUndefined();
  });

  test('withdrawn inputs reject with the visible retry message', async () => {
    let state = displayState();
    const tracker = new DeliveryTracker(() => state);
    const waiting = tracker.wait('input-1');
    tracker.settle(queue([{ inputId: 'input-1', state: 'withdrawn' }]));
    await expect(waiting).rejects.toThrow(INPUT_WITHDRAWN_MESSAGE);
  });

  test('unrelated settles leave other inputs pending', async () => {
    let state = displayState();
    const tracker = new DeliveryTracker(() => state);
    let first = false;
    const one = tracker.wait('input-1').then(() => { first = true; });
    const two = tracker.wait('input-2');
    tracker.settle(queue([{ inputId: 'input-1', state: 'delivered' }]));
    await one;
    expect(first).toBe(true);
    state = queue([{ inputId: 'input-1', state: 'delivered' }, { inputId: 'input-2', state: 'delivered' }]);
    tracker.settle(state);
    await expect(two).resolves.toBeUndefined();
  });

  test('releasing the connection rejects every pending wait and clears the table', async () => {
    const tracker = new DeliveryTracker(() => displayState());
    const one = tracker.wait('input-1');
    const two = tracker.wait('input-2');
    tracker.rejectAll();
    await expect(one).rejects.toThrow(CONNECTION_RELEASED_MESSAGE);
    await expect(two).rejects.toThrow(CONNECTION_RELEASED_MESSAGE);
    // 已清空后再次结算不会重复拒绝或抛错。
    expect(() => tracker.settle(displayState())).not.toThrow();
  });

  test('settling an empty table is a no-op', () => {
    const tracker = new DeliveryTracker(() => displayState());
    expect(() => tracker.settle(queue([{ inputId: 'input-1', state: 'delivered' }]))).not.toThrow();
  });
});
