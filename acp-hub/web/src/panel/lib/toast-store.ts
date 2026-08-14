import { createSignal, type Accessor } from 'solid-js';

export interface ToastRecord {
  id: number;
  msg: string;
}

/** Owns transient notification state and every expiry callback that can mutate it. */
export class ToastStore {
  private readonly recordsState = createSignal<ToastRecord[]>([]);
  private readonly timers = new Map<number, ReturnType<typeof setTimeout>>();
  private sequence = 0;

  readonly records: Accessor<ToastRecord[]> = this.recordsState[0];

  constructor(private readonly durationMs = 2500) {}

  show(msg: string): number {
    const id = ++this.sequence;
    this.recordsState[1]((records) => [...records, { id, msg }]);
    const timer = setTimeout(() => {
      this.timers.delete(id);
      this.recordsState[1]((records) => records.filter((record) => record.id !== id));
    }, this.durationMs);
    this.timers.set(id, timer);
    return id;
  }

  clear(): void {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
    this.recordsState[1]([]);
  }
}
