import * as Y from "yjs";
import {
    decodeSyncFrame, encodeAckFrame, encodeHelloFrame, SessionDocReplica,
} from "../sync/index";
import { ReadCache } from "./read-cache";
import type { ChatYjsChannel } from "./channel";
import { readSessionView, type SessionView } from "./session-view";

export type ChatReplicaStatus = "connecting" | "synced" | "failed";
export type ChatReplicaConnection = { close(): Promise<void> };

/**
 * 浏览器侧只读副本：收发 HELLO/ACK，应用 SNAPSHOT/UPDATE，提供选择器可用的 SessionView。
 *
 * 副本从不修改 Y.Doc 来表达业务意图；状态/就绪度分开暴露，schema 不支持不当作空历史。
 */
export class ChatReplica {
    private readonly replica = new SessionDocReplica();
    private chat = this.replica.chat;
    private session = this.replica.session;
    private cache = new ReadCache();
    private view: SessionView = this.recompute();
    private readonly listeners = new Set<() => void>();
    private channel: ChatYjsChannel | null = null;
    private detachMessage: (() => void) | null = null;
    private detachClose: (() => void) | null = null;
    private statusValue: ChatReplicaStatus = "connecting";
    private closed = false;
    private disposed = false;
    private scheduled = false;
    private readonly onTransaction = (transaction: Y.Transaction): void => {
        if (!transaction.changed.size) return;
        if ([...transaction.changed.keys()].some((type) => type.parent === null)) this.cache = new ReadCache();
        else this.cache.invalidate(transaction);
        this.schedule();
    };

    get status(): ChatReplicaStatus { return this.statusValue; }

    constructor() {
        this.attach();
    }

    connect(channel: ChatYjsChannel): ChatReplicaConnection {
        if (this.disposed) throw new Error("ChatReplica is disposed");
        if (this.channel) throw new Error("ChatReplica is already connected; close the previous connection first");
        this.closed = false;
        this.statusValue = "connecting";
        this.channel = channel;
        this.detachMessage = channel.onMessage((frame) => this.receive(frame));
        this.detachClose = channel.onClose((error) => this.fail(error ?? new Error("Chat sync channel closed")));
        const vector = this.replica.stateVector();
        this.send(encodeHelloFrame({ type: "hello", ...(vector ? { resume: vector } : {}) }));
        return { close: () => this.closeConnection() };
    }

    read(): SessionView {
        return this.view;
    }

    subscribe(listener: () => void): () => void {
        if (this.disposed) return () => {};
        this.listeners.add(listener);
        return () => this.listeners.delete(listener);
    }

    async dispose(): Promise<void> {
        if (this.disposed) return;
        this.disposed = true;
        await this.closeConnection();
        this.listeners.clear();
        this.detach();
        this.replica.destroy();
    }

    private receive(raw: Uint8Array): void {
        if (this.closed || this.disposed) return;
        try {
            const frame = decodeSyncFrame(raw);
            const previousChat = this.replica.chat;
            const previousSession = this.replica.session;
            if (frame.type === "snapshot") this.replica.applySnapshot(frame.snapshot);
            else this.replica.applyUpdate(frame.update);
            if (previousChat !== this.replica.chat || previousSession !== this.replica.session) {
                this.detach();
                this.chat = this.replica.chat;
                this.session = this.replica.session;
                this.cache = new ReadCache();
                this.attach();
            }
            this.statusValue = "synced";
            this.send(encodeAckFrame(frame.delivery));
            this.schedule();
        } catch (error) {
            this.fail(error instanceof Error ? error : new Error("Invalid chat sync frame"));
        }
    }

    private send(frame: Uint8Array): void {
        const channel = this.channel;
        if (!channel || channel.closed) {
            this.fail(new Error("Chat sync channel is closed"));
            return;
        }
        try {
            const pending = channel.send(frame);
            if (pending) void Promise.resolve(pending).catch((error: unknown) => {
                this.fail(error instanceof Error ? error : new Error("Chat sync send failed"));
            });
        } catch (error) {
            this.fail(error instanceof Error ? error : new Error("Chat sync send failed"));
        }
    }

    private fail(error: Error): void {
        if (this.disposed) return;
        this.statusValue = "failed";
        const channel = this.channel;
        this.cleanupConnection();
        this.emit();
        if (channel && !channel.closed) {
            void Promise.resolve(channel.close(error)).catch(() => {});
        }
    }

    private async closeConnection(): Promise<void> {
        if (!this.channel) return;
        const channel = this.channel;
        this.cleanupConnection();
        this.statusValue = "failed";
        this.emit();
        if (!channel.closed) await channel.close();
    }

    private cleanupConnection(): void {
        this.closed = true;
        this.detachMessage?.();
        this.detachClose?.();
        this.detachMessage = null;
        this.detachClose = null;
        this.channel = null;
    }

    private attach(): void {
        this.chat.on("afterTransaction", this.onTransaction);
        this.session.on("afterTransaction", this.onTransaction);
    }

    private detach(): void {
        this.chat.off("afterTransaction", this.onTransaction);
        this.session.off("afterTransaction", this.onTransaction);
    }

    private schedule(): void {
        if (this.scheduled || this.disposed) return;
        this.scheduled = true;
        queueMicrotask(() => {
            this.scheduled = false;
            if (this.disposed) return;
            this.view = this.recompute();
            this.emit();
        });
    }

    private recompute(): SessionView {
        return readSessionView(this.chat, this.session, this.cache);
    }

    private emit(): void {
        for (const listener of this.listeners) listener();
    }
}
