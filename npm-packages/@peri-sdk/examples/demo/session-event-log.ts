import type { JsonRpcNotification } from "../../src/transport/types";

export type SessionEvent = { id: number; notification: JsonRpcNotification };

/** In-process replay buffer for one demo Session. */
export class SessionEventLog {
  private nextId = 1;
  private readonly entries: SessionEvent[] = [];
  private readonly listeners = new Set<(event: SessionEvent) => void>();

  append(notification: JsonRpcNotification): void {
    const entry = { id: this.nextId++, notification };
    this.entries.push(entry);
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
