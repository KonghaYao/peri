import type { JsonRpcNotification } from "../../src/transport/types";
import { notificationBytes } from "../../src/transport/notification-budget";

export type SessionEvent = { id: number; notification: JsonRpcNotification };

/** Bounded diagnostic replay; SessionDocs is the current-state projection. */
export class SessionEventLog {
  private nextId = 1;
  private readonly entries: SessionEvent[] = [];
  private readonly sizes: number[] = [];
  private bytes = 0;
  private readonly listeners = new Set<(event: SessionEvent) => void>();

  append(notification: JsonRpcNotification): void {
    const entry = { id: this.nextId++, notification };
    const size = notificationBytes(notification, 1024 * 1024);
    if (size > 1024 * 1024) return;
    while (this.entries.length >= 256 || this.bytes + size > 1024 * 1024) {
      this.entries.shift();
      this.bytes -= this.sizes.shift()!;
    }
    this.entries.push(entry);
    this.sizes.push(size);
    this.bytes += size;
    for (const listener of this.listeners) listener(entry);
  }

  /** Replay first, then follow live events without a gap between the two. */
  subscribe(after: number, listener: (event: SessionEvent) => void): () => void {
    for (const entry of this.entries) {
      if (entry.id > after) listener(entry);
    }
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
}
