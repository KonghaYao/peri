import { afterEach, describe, expect, it, vi } from 'vitest';
import { ToastStore } from './toast-store';

afterEach(() => {
  vi.useRealTimers();
});

describe('ToastStore', () => {
  it('expires only the notification owned by its timer', () => {
    vi.useFakeTimers();
    const store = new ToastStore(2500);

    store.show('first');
    vi.advanceTimersByTime(1000);
    store.show('second');
    vi.advanceTimersByTime(1500);

    expect(store.records().map((item) => item.msg)).toEqual(['second']);
    vi.advanceTimersByTime(1000);
    expect(store.records()).toEqual([]);
  });

  it('cancels every old-principal callback when cleared', () => {
    vi.useFakeTimers();
    const store = new ToastStore(2500);
    store.show('private old-session feedback');

    store.clear();
    store.show('new-session feedback');
    vi.advanceTimersByTime(2499);

    expect(store.records().map((item) => item.msg)).toEqual(['new-session feedback']);
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(1);
    expect(store.records()).toEqual([]);
  });
});
