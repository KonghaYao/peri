import { describe, expect, it, vi } from 'vitest';
import { runConfirmedMutation } from './form-mutation';

describe('runConfirmedMutation', () => {
  it('keeps destructive cleanup behind committed delivery', () => {
    const start = vi.fn();
    const stop = vi.fn();
    const commit = vi.fn();
    let committed!: () => void;
    let failed!: () => void;
    runConfirmedMutation(start, stop, (onCommitted, onFailed) => {
      committed = onCommitted; failed = onFailed; return true;
    }, commit);

    expect(start).toHaveBeenCalledOnce();
    expect(stop).not.toHaveBeenCalled();
    expect(commit).not.toHaveBeenCalled();
    failed();
    committed();
    committed();
    expect(stop).toHaveBeenCalledTimes(2);
    expect(commit).toHaveBeenCalledOnce();
  });

  it('unlocks immediately when dispatch fails and commits exactly once', () => {
    const unlockFailed = vi.fn();
    expect(runConfirmedMutation(vi.fn(), unlockFailed, () => false, vi.fn())).toBe(false);
    expect(unlockFailed).toHaveBeenCalledOnce();

    const stop = vi.fn();
    const commit = vi.fn();
    runConfirmedMutation(vi.fn(), stop, (onCommitted) => { onCommitted(); onCommitted(); return true; }, commit);
    expect(stop).toHaveBeenCalledOnce();
    expect(commit).toHaveBeenCalledOnce();
  });
});
