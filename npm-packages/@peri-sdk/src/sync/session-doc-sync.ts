import * as Y from "yjs";
import { SyncPeer } from "./peer";

export type DocStateVector = { protocol: 2; generation: string; chat: Uint8Array; session: Uint8Array };
export type DocSnapshot = {
    protocol: 2; generation: string; sequence: number; mode: "snapshot" | "delta";
    chat: Uint8Array; session: Uint8Array;
};
export type DocUpdate = {
    protocol: 2; generation: string; sequence: number;
    chat?: Uint8Array; session?: Uint8Array;
};
export type DocSyncOptions = {
    flushIntervalMs?: number;
    maxBatchBytes?: number;
    maxBatchUpdates?: number;
    maxSubscriberBytes?: number;
};

function positive(value: number, name: string): number {
    if (!Number.isSafeInteger(value) || value <= 0) throw new RangeError(`${name} must be a positive safe integer`);
    return value;
}

/** Authorized read-only replication. ACP remains the command boundary; peers never write back. */
export class SessionDocSync {
    readonly generation = crypto.randomUUID();
    private sequence = 0;
    private closed = false;
    private closePromise: Promise<void> | undefined;
    private flushing = false;
    private flushAgain = false;
    private readonly peers = new Set<SyncPeer>();
    private pending: { chat: Uint8Array[]; session: Uint8Array[] } = { chat: [], session: [] };
    private pendingBytes = 0;
    private pendingCount = 0;
    private timer: ReturnType<typeof setTimeout> | undefined;
    private readonly interval: number;
    private readonly maxBytes: number;
    private readonly maxUpdates: number;
    private readonly subscriberBytes: number;
    private readonly onChat = (update: Uint8Array) => this.enqueue("chat", update);
    private readonly onSession = (update: Uint8Array) => this.enqueue("session", update);
    private readonly onDestroy = () => { void this.close(); };

    constructor(private readonly chat: Y.Doc, private readonly session: Y.Doc, options: DocSyncOptions = {}) {
        this.interval = positive(options.flushIntervalMs ?? 16, "flushIntervalMs");
        this.maxBytes = positive(options.maxBatchBytes ?? 64 * 1024, "maxBatchBytes");
        this.maxUpdates = positive(options.maxBatchUpdates ?? 256, "maxBatchUpdates");
        this.subscriberBytes = positive(options.maxSubscriberBytes ?? 4 * 1024 * 1024, "maxSubscriberBytes");
        chat.on("destroy", this.onDestroy);
        session.on("destroy", this.onDestroy);
    }

    /** Snapshot and listener admission are synchronous, so no update can fall between them. */
    subscribe(send: (update: DocUpdate) => void | Promise<void>, options: {
        resume?: DocStateVector; onError?: (error: Error) => void;
    } = {}): { snapshot: DocSnapshot; unsubscribe: () => void } {
        if (this.closed) throw new Error("Yjs sync is closed");
        this.flush();
        // Flushing calls existing transports synchronously; one may close the producer.
        if (this.closed) throw new Error("Yjs sync is closed");
        const resume = options.resume;
        const delta = resume?.protocol === 2 && resume.generation === this.generation;
        if (delta && (resume.chat.byteLength > 64 * 1024 || resume.session.byteLength > 64 * 1024)) {
            throw new RangeError("Yjs state vector exceeds the byte budget");
        }
        const snapshot: DocSnapshot = {
            protocol: 2, generation: this.generation, sequence: this.sequence,
            mode: delta ? "delta" : "snapshot",
            chat: Y.encodeStateAsUpdateV2(this.chat, delta ? resume.chat : undefined),
            session: Y.encodeStateAsUpdateV2(this.session, delta ? resume.session : undefined),
        };
        const peer = new SyncPeer(send, this.subscriberBytes, (error) => {
            this.peers.delete(peer);
            if (!this.peers.size) this.detachUpdates();
            if (error) { try { options.onError?.(error); } catch { /* isolate subscriber callbacks */ } }
        });
        if (!this.peers.size) {
            // V1 is cheaper for tiny transactions. Convert once per merged batch for the wire.
            this.chat.on("update", this.onChat);
            this.session.on("update", this.onSession);
        }
        this.peers.add(peer);
        return { snapshot, unsubscribe: () => peer.close() };
    }

    /** Publish tail deltas before reporting a terminal boundary or stopping a transport. */
    flush(): void {
        if (this.flushing) { this.flushAgain = true; return; }
        this.flushing = true;
        try {
            do {
                this.flushAgain = false;
                this.publishPending();
            } while (this.flushAgain && this.pendingCount);
        } finally { this.flushing = false; }
    }

    private publishPending(): void {
        if (this.timer !== undefined) { clearTimeout(this.timer); this.timer = undefined; }
        if (!this.pendingCount) return;
        const pending = this.pending;
        this.pending = { chat: [], session: [] };
        this.pendingBytes = 0;
        this.pendingCount = 0;
        const merged = (updates: Uint8Array[]) => Y.convertUpdateFormatV1ToV2(
            updates.length === 1 ? updates[0]! : Y.mergeUpdates(updates));
        const update: DocUpdate = {
            protocol: 2, generation: this.generation, sequence: ++this.sequence,
            ...(pending.chat.length ? { chat: merged(pending.chat) } : {}),
            ...(pending.session.length ? { session: merged(pending.session) } : {}),
        };
        const peers = [...this.peers];
        for (let index = 0; index < peers.length; index++) {
            // Each adapter owns its bytes, including the right to transfer them to a Worker.
            const owned = index === peers.length - 1 ? update : { ...update,
                ...(update.chat ? { chat: update.chat.slice() } : {}),
                ...(update.session ? { session: update.session.slice() } : {}) };
            peers[index]!.push(owned);
        }
    }

    close(): Promise<void> {
        if (this.closePromise) return this.closePromise;
        this.closed = true;
        let resolve!: () => void;
        this.closePromise = new Promise((done) => { resolve = done; });
        const finish = () => {
            this.flush();
            this.detachUpdates();
            this.chat.off("destroy", this.onDestroy);
            this.session.off("destroy", this.onDestroy);
            void Promise.all([...this.peers].map((peer) => peer.finish())).then(() => resolve());
        };
        // A callback may close its producer while another peer still awaits the current frame.
        if (this.flushing) queueMicrotask(finish);
        else finish();
        return this.closePromise;
    }

    private detachUpdates(): void {
        this.chat.off("update", this.onChat);
        this.session.off("update", this.onSession);
        if (this.timer !== undefined) { clearTimeout(this.timer); this.timer = undefined; }
        this.pending = { chat: [], session: [] };
        this.pendingBytes = 0;
        this.pendingCount = 0;
    }

    private enqueue(doc: "chat" | "session", update: Uint8Array): void {
        if (this.closed || !this.peers.size) return;
        // One indivisible Yjs update may exceed the target; subscriber budgets still apply.
        if (this.pendingBytes && this.pendingBytes + update.byteLength > this.maxBytes) this.flush();
        if (!this.peers.size) return;
        this.pending[doc].push(update);
        this.pendingBytes += update.byteLength;
        this.pendingCount++;
        if (this.pendingBytes >= this.maxBytes || this.pendingCount >= this.maxUpdates) this.flush();
        else if (this.timer === undefined) this.timer = setTimeout(() => this.flush(), this.interval);
    }
}
