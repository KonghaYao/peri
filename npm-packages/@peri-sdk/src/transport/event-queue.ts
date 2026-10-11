import type { JsonRpcNotification } from "./types";
import { EventStreamOverflowError, notificationBytes } from "./notification-budget";

export class EventQueue implements AsyncIterableIterator<JsonRpcNotification> {
  private items: Array<{ value: JsonRpcNotification; bytes: number }> = [];
  private head = 0;
  private bytes = 0;
  private waiters: Array<{ resolve: (result: IteratorResult<JsonRpcNotification>) => void; reject: (error: Error) => void }> = [];
  private done = false;
  private error: Error | undefined;

  constructor(private readonly onFinish: () => void) {}

  push(value: JsonRpcNotification): void {
    if (this.done) return;
    const waiter = this.waiters.shift();
    if (waiter) waiter.resolve({ value, done: false });
    else {
      const bytes = notificationBytes(value, 4 * 1024 * 1024);
      if (this.items.length - this.head >= 1024 || this.bytes + bytes > 4 * 1024 * 1024) {
        this.error = new EventStreamOverflowError();
        this.items = [];
        this.head = this.bytes = 0;
        this.finish();
        return;
      }
      this.items.push({ value, bytes });
      this.bytes += bytes;
    }
  }

  next(): Promise<IteratorResult<JsonRpcNotification>> {
    if (this.error) return Promise.reject(this.error);
    const item = this.take();
    if (item) return Promise.resolve({ value: item, done: false });
    if (this.done) return Promise.resolve({ value: undefined, done: true });
    return new Promise((resolve, reject) => this.waiters.push({ resolve, reject }));
  }

  return(): Promise<IteratorResult<JsonRpcNotification>> {
    this.items = [];
    this.head = this.bytes = 0;
    this.error = undefined;
    this.finish();
    return Promise.resolve({ value: undefined, done: true });
  }

  finish(): void {
    if (this.done) return;
    this.done = true;
    this.onFinish();
    for (const waiter of this.waiters.splice(0)) {
      if (this.error) waiter.reject(this.error);
      else {
        const item = this.take();
        waiter.resolve(item ? { value: item, done: false } : { value: undefined, done: true });
      }
    }
  }

  private take(): JsonRpcNotification | undefined {
    const item = this.items[this.head];
    if (!item) return undefined;
    this.bytes -= item.bytes;
    this.items[this.head++] = undefined as never;
    if (this.head === this.items.length) { this.items = []; this.head = 0; }
    else if (this.head >= 256) { this.items = this.items.slice(this.head); this.head = 0; }
    return item.value;
  }

  [Symbol.asyncIterator](): AsyncIterableIterator<JsonRpcNotification> { return this; }
}
