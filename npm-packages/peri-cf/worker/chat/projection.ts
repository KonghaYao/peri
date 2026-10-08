import { SessionDocs, SessionDocSync, SessionViewStore, type JsonRpcNotification } from "../sdk";
import type { Message, SessionRecord } from "../types";
import { syncMetadataSchema, type SyncMetadata } from "../../shared/sync";
import { materializeSyncState } from "../../shared/sync-state";

export class ChatProjection {
  readonly docs = new SessionDocs();
  readonly sync = new SessionDocSync(this.docs.chat, this.docs.session);
  private readonly view = new SessionViewStore(this.docs.chat, this.docs.session);
  private readonly presentation = new Map<string, Pick<Message, "id" | "createdAt" | "error">>();
  private record?: SessionRecord;
  private error?: string;
  private published = "";
  private commandInputDelivered = false;
  private metadataDirty = true;
  private publishedEntryCount = -1;
  private metadata?: SyncMetadata;

  constructor() {
    this.view.subscribe(() => this.publish());
  }

  async restore(record: SessionRecord): Promise<void> {
    this.record = record;
    this.metadataDirty = true;
    for (const message of record.messages) this.presentation.set(`message:${message.id}`, {
      id: message.id, createdAt: message.createdAt, ...(message.error ? { error: message.error } : {}),
    });
    let turnId: string | undefined;
    let assistantId: string | undefined;
    let terminal: "completed" | "cancelled" | "error" = "completed";
    const firstUser = record.messages.findIndex((message) => message.role === "user");
    const leading = record.messages.slice(0, firstUser < 0 ? record.messages.length : firstUser);
    if (leading.length) this.docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: {
      sessionId: record.chat.id, event_json: JSON.stringify({ type: "compact_completed", value: {
        messages_json: JSON.stringify(leading),
      } }),
    } });
    for (const message of record.messages) {
      if (message.role === "user") {
        if (turnId) this.docs.completeTurn(terminal);
        turnId = `turn:${message.id}`;
        assistantId = undefined;
        terminal = "completed";
        this.docs.acceptDeliveredUserInput(message.id, message.content);
        this.presentation.set(`${turnId}:user`, message);
      } else if (turnId) {
        this.docs.accept({ jsonrpc: "2.0", method: "session/update", params: {
          sessionId: record.chat.id, update: { sessionUpdate: "agent_message_chunk", messageId: message.id,
            content: { type: "text", text: message.content } },
        } });
        this.presentation.set(assistantId ? `${turnId}:assistant:${message.id}` : `${turnId}:assistant`, message);
        assistantId = message.id;
        terminal = message.status === "running" ? "error" : message.status;
      }
    }
    if (turnId) this.docs.completeTurn(terminal);
    await this.flush();
  }

  async start(content: string): Promise<void> {
    this.error = undefined;
    this.commandInputDelivered = true;
    this.docs.acceptDeliveredUserInput(crypto.randomUUID(), content);
    await this.flush();
  }

  accept(notification: JsonRpcNotification): void {
    const params = notification.params as { update?: { sessionUpdate?: unknown } } | undefined;
    if (this.commandInputDelivered && notification.method === "session/update"
      && params?.update?.sessionUpdate === "user_message_chunk") return;
    if (notification.method !== "session/update"
      || !["agent_message_chunk", "agent_thought_chunk"].includes(String(params?.update?.sessionUpdate))) {
      this.metadataDirty = true;
    }
    this.docs.accept(notification);
  }

  async complete(status: "completed" | "cancelled" | "error", error?: string): Promise<void> {
    this.error = error;
    this.commandInputDelivered = false;
    this.docs.completeTurn(status);
    await this.flush();
  }

  async flush(): Promise<void> {
    await Promise.resolve();
    this.metadataDirty = true;
    this.publish();
    if (this.record && this.metadata) {
      this.record.messages = materializeSyncState(this.view.getSnapshot(), this.metadata).messages.map((message) => ({
        ...message, status: message.status!,
      }));
    }
    this.sync.flush();
  }

  private publish(): void {
    if (!this.record) return;
    const snapshot = this.view.getSnapshot();
    if (!this.metadataDirty && this.publishedEntryCount === snapshot.entries.length) return;
    this.metadataDirty = false;
    this.publishedEntryCount = snapshot.entries.length;
    const entryMetadata = Object.fromEntries(snapshot.entries.map((entry) => {
      let presentation = this.presentation.get(entry.entryId);
      if (!presentation) {
        presentation = { id: entry.messageId ?? crypto.randomUUID(), createdAt: new Date().toISOString() };
        this.presentation.set(entry.entryId, presentation);
      }
      const active = entry.turnId === snapshot.activeTurnId;
      const error = active && entry.role === "assistant" && this.error ? this.error : presentation.error;
      if (error) presentation.error = error;
      return [entry.entryId, { id: presentation.id, createdAt: presentation.createdAt, ...(error ? { error } : {}) }];
    }));
    const state: SyncMetadata = { chat: this.record.chat, entryMetadata, running: !!this.record.running,
      executionBlocked: !!this.record.executionBlocked, ...(this.error ? { error: this.error } : {}) };
    const serialized = JSON.stringify(state);
    this.metadata = state;
    if (serialized !== this.published) {
      this.published = serialized;
      this.docs.session.getMap("peri-cf").set("state", syncMetadataSchema.parse(state));
    }
  }
}
