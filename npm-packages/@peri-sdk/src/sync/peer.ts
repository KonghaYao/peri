import type { DocUpdate } from "./session-doc-sync";

export class SyncBackpressureError extends Error {
    constructor() { super("Yjs subscriber exceeded its byte budget; reconnect with state vectors"); this.name = "SyncBackpressureError"; }
}

/** One slow transport cannot retain an unbounded queue or block other subscribers. */
export class SyncPeer {
    private queue: DocUpdate[] = [];
    private head = 0;
    private bytes = 0;
    private sending = false;
    private closed = false;
    private finishing = false;
    private readonly drained: Promise<void>;
    private resolveDrained!: () => void;

    constructor(
        private readonly send: (update: DocUpdate) => void | Promise<void>,
        private readonly maxBytes: number,
        private readonly onClose: (error?: Error) => void,
    ) { this.drained = new Promise((resolve) => { this.resolveDrained = resolve; }); }

    push(update: DocUpdate): void {
        if (this.closed || this.finishing) return;
        const bytes = (update.chat?.byteLength ?? 0) + (update.session?.byteLength ?? 0);
        if (this.bytes + bytes > this.maxBytes) { this.close(new SyncBackpressureError()); return; }
        this.bytes += bytes;
        this.queue.push(update);
        this.pump();
    }

    close(error?: Error): void {
        if (this.closed) return;
        this.closed = true;
        this.queue = [];
        this.head = 0;
        this.bytes = 0;
        this.resolveDrained();
        this.onClose(error);
    }

    finish(): Promise<void> {
        this.finishing = true;
        this.pump();
        return this.drained;
    }

    private pump(): void {
        if (this.sending || this.closed) return;
        while (this.head < this.queue.length) {
            const update = this.queue[this.head++]!;
            const bytes = (update.chat?.byteLength ?? 0) + (update.session?.byteLength ?? 0);
            try {
                this.sending = true;
                const pending = this.send(update);
                if (pending) {
                    // Even an adapter that synchronously unsubscribes still owns a returned rejection.
                    void Promise.resolve(pending).then(() => {
                        if (this.closed) return;
                        this.bytes -= bytes;
                        this.sending = false;
                        this.pump();
                    }, (error: unknown) => this.close(error instanceof Error ? error : new Error("Yjs subscriber failed")));
                    this.compact();
                    return;
                }
                if (this.closed) return;
                this.sending = false;
                this.bytes -= bytes;
            } catch (error) {
                this.close(error instanceof Error ? error : new Error("Yjs subscriber failed"));
                return;
            }
        }
        this.compact();
        if (this.finishing) this.close();
    }

    private compact(): void {
        if (this.head) { this.queue = this.queue.slice(this.head); this.head = 0; }
    }
}
