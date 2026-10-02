import type { JsonRpcNotification } from "./types";

export class EventQueue implements AsyncIterableIterator<JsonRpcNotification> {
  private items: JsonRpcNotification[] = [];
  private waiters: Array<(result: IteratorResult<JsonRpcNotification>) => void> = [];
  private done = false;

  constructor(private readonly onFinish: () => void) {}

  push(value: JsonRpcNotification): void {
    if (this.done) return;
    const waiter = this.waiters.shift();
    if (waiter) waiter({ value, done: false });
    else this.items.push(value);
  }

  next(): Promise<IteratorResult<JsonRpcNotification>> {
    const item = this.items.shift();
    if (item) return Promise.resolve({ value: item, done: false });
    if (this.done) return Promise.resolve({ value: undefined, done: true });
    return new Promise((resolve) => this.waiters.push(resolve));
  }

  return(): Promise<IteratorResult<JsonRpcNotification>> {
    this.items = [];
    this.finish();
    return Promise.resolve({ value: undefined, done: true });
  }

  finish(): void {
    if (this.done) return;
    this.done = true;
    this.onFinish();
    for (const waiter of this.waiters.splice(0)) {
      const item = this.items.shift();
      waiter(item ? { value: item, done: false } : { value: undefined, done: true });
    }
  }

  [Symbol.asyncIterator](): AsyncIterableIterator<JsonRpcNotification> { return this; }
}
