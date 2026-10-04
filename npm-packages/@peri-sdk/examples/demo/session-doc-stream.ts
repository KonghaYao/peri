import * as Y from "yjs";

export type DocName = "chat" | "session";

export type DocSnapshot = {
  generation: string;
  sequence: number;
  chat: string;
  session: string;
};

export type DocUpdate = {
  generation: string;
  sequence: number;
  doc: DocName;
  update: string;
};

function base64(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64");
}

/** Process-local bridge for the loopback demo. Every connection starts with current Y.Doc state. */
export class SessionDocStream {
  readonly generation = crypto.randomUUID();
  private sequence = 0;
  private readonly listeners = new Set<(event: DocUpdate) => void>();
  private readonly onChatUpdate = (update: Uint8Array) => this.publish("chat", update);
  private readonly onSessionUpdate = (update: Uint8Array) => this.publish("session", update);

  constructor(private readonly chat: Y.Doc, private readonly session: Y.Doc) {
    chat.on("update", this.onChatUpdate);
    session.on("update", this.onSessionUpdate);
  }

  /** Capture both docs and register the listener in one synchronous step. */
  subscribe(listener: (event: DocUpdate) => void): { snapshot: DocSnapshot; unsubscribe: () => void } {
    const snapshot: DocSnapshot = {
      generation: this.generation,
      sequence: this.sequence,
      chat: base64(Y.encodeStateAsUpdate(this.chat)),
      session: base64(Y.encodeStateAsUpdate(this.session)),
    };
    this.listeners.add(listener);
    return { snapshot, unsubscribe: () => this.listeners.delete(listener) };
  }

  close(): void {
    this.chat.off("update", this.onChatUpdate);
    this.session.off("update", this.onSessionUpdate);
    this.listeners.clear();
  }

  private publish(doc: DocName, update: Uint8Array): void {
    const event: DocUpdate = {
      generation: this.generation,
      sequence: ++this.sequence,
      doc,
      update: base64(update),
    };
    for (const listener of this.listeners) listener(event);
  }
}
