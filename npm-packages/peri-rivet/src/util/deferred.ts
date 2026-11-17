export interface Deferred<T> {
  readonly promise: Promise<T>;
  readonly settled: boolean;
  resolve(value: T | PromiseLike<T>): void;
  reject(reason: unknown): void;
}

/**
 * A promise whose settlement is idempotent.
 *
 * Activity leases can be released from `finally` blocks and host shutdown paths; making settlement
 * idempotent removes a class of double-settle races without hiding state transitions.
 */
export function createDeferred<T>(): Deferred<T> {
  let settle!: (value: T | PromiseLike<T>) => void;
  let fail!: (reason: unknown) => void;
  let settled = false;

  const promise = new Promise<T>((resolve, reject) => {
    settle = resolve;
    fail = reject;
  });

  return {
    promise,
    get settled(): boolean {
      return settled;
    },
    resolve(value): void {
      if (settled) {
        return;
      }
      settled = true;
      settle(value);
    },
    reject(reason): void {
      if (settled) {
        return;
      }
      settled = true;
      fail(reason);
    },
  };
}
